//! egui 软件后端：把 epaint 网格光栅化成**预乘 RGBA**，供分层窗口（UpdateLayeredWindow）上屏。
//!
//! 这些窗口依赖 NOACTIVATE + 逐像素透明 + 圆角命中，不能直接换成 eframe 原生窗口。
//! 本模块把 pillui.rs 已验证的"egui 布局 → epaint 网格 → 自写光栅化 → 预乘 RGBA"模式
//! 抽成公共后端，供胶囊 / Dock / 模块条 / 音乐条 / 面板共用。
//!
//! 注意（本项目踩过的坑）：
//! - 输出是**预乘 alpha**，整体透明度必须连 RGB 一起乘（调用方负责）；
//! - 字体纹理与图标纹理都在预乘域，直接分量相乘即可正确合成；
//! - egui 顶层面板会画不透明的 panel_fill 底，必须在样式里置成 TRANSPARENT。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use eframe::egui::epaint;
use eframe::egui::epaint::Primitive;

use crate::config as cfg;
use crate::settings;

// ---------- 纹理数据 ----------

/// 一张已解码为预乘域的 RGBA 纹理（epaint 网格采样源）。
pub struct TexData {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>,
}

/// 把 egui 上传的 ColorImage 转成我们的 TexData（只认 Color 变体，Font 由 egui 内部处理）。
pub fn image_to_tex(img: &egui::ImageData) -> Option<TexData> {
    let egui::ImageData::Color(ci) = img;
    let [w, h] = ci.size;
    if w == 0 || h == 0 {
        return None;
    }
    let mut px = Vec::with_capacity(w * h * 4);
    for c in &ci.pixels {
        px.extend_from_slice(&c.to_array());
    }
    Some(TexData { w: w as i32, h: h as i32, px })
}

/// 把一张 ImageDelta 合并进已有的 TexData。
///
/// egui 的字体图集是**局部更新**（`pos = Some([x, y])` 表示只更新那一块），
/// 不能当成整张纹理替换，否则会丢掉其它字形。这里按 pos 逐块拷贝。
fn merge_delta(tex: &mut TexData, img: &egui::ImageData, pos: Option<[usize; 2]>) {
    let egui::ImageData::Color(ci) = img;
    let [w, h] = ci.size;
    if w == 0 || h == 0 {
        return;
    }
    match pos {
        None => {
            // 整张替换
            tex.w = w as i32;
            tex.h = h as i32;
            tex.px.clear();
            tex.px.reserve(w * h * 4);
            for c in &ci.pixels {
                tex.px.extend_from_slice(&c.to_array());
            }
        }
        Some([px0, py0]) => {
            // 局部更新：目标必须放得下，否则扩容后拷贝
            let need_w = (px0 + w) as i32;
            let need_h = (py0 + h) as i32;
            if need_w > tex.w || need_h > tex.h {
                let new_w = tex.w.max(need_w);
                let new_h = tex.h.max(need_h);
                let mut np = vec![0u8; (new_w * new_h * 4) as usize];
                // 拷回旧内容
                for y in 0..tex.h.min(new_h) {
                    for x in 0..tex.w.min(new_w) {
                        let si = ((y * tex.w + x) * 4) as usize;
                        let di = ((y * new_w + x) * 4) as usize;
                        np[di..di + 4].copy_from_slice(&tex.px[si..si + 4]);
                    }
                }
                tex.w = new_w;
                tex.h = new_h;
                tex.px = np;
            }
            for y in 0..h {
                for x in 0..w {
                    let si = (y * w + x) * 4;
                    let di = (((py0 + y) * tex.w as usize) + (px0 + x)) * 4;
                    if si + 3 >= ci.pixels.len() * 4 || di + 3 >= tex.px.len() {
                        continue;
                    }
                    let c = ci.pixels[y * w + x].to_array();
                    tex.px[di..di + 4].copy_from_slice(&c);
                }
            }
        }
    }
}

/// 双线性采样（uv 是 epaint 约定的纹理像素坐标）。
fn sample_tex(tex: &TexData, u: f32, v: f32) -> [f32; 4] {
    let x = (u - 0.5).clamp(0.0, tex.w as f32 - 1.0);
    let y = (v - 0.5).clamp(0.0, tex.h as f32 - 1.0);
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let (x1, y1) = ((x0 + 1).min(tex.w - 1), (y0 + 1).min(tex.h - 1));
    let at = |px: i32, py: i32| -> [f32; 4] {
        let i = ((py * tex.w + px) * 4) as usize;
        if i + 3 < tex.px.len() {
            [
                tex.px[i] as f32,
                tex.px[i + 1] as f32,
                tex.px[i + 2] as f32,
                tex.px[i + 3] as f32,
            ]
        } else {
            [0.0; 4]
        }
    };
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    let mut out = [0.0f32; 4];
    for k in 0..4 {
        out[k] = a[k] * (1.0 - fx) * (1.0 - fy)
            + b[k] * fx * (1.0 - fy)
            + c[k] * (1.0 - fx) * fy
            + d[k] * fx * fy;
    }
    out
}

/// 一个三角形的光栅化：重心插值 uv/颜色，预乘 over 合成。
/// epaint 的抗锯齿靠"羽化边"（几何外扩 + 顶点色透明渐变），
/// 所以这里不需要逐像素 AA，直接硬边填充即可。
///
/// `snap_px`：把顶点对齐到物理像素网格。字体图集网格（文字）必须开——
/// epaint 只对 galley 整体取整，字形在 galley 内部仍是亚像素摆放，
/// 双线性采样会把已经抗锯齿的字形再糊一遍（小字号下尤其明显，
/// 表现为天气卡片的小字发虚）。逐字形对齐后采样 1:1 命中纹素，文字恢复锐利。
pub fn fill_tri(
    buf: &mut [u8],
    w: i32,
    h: i32,
    clip: (f32, f32, f32, f32), // 物理像素 x0 y0 x1 y1
    v: [&egui::epaint::Vertex; 3],
    ppp: f32,
    tex: Option<&TexData>,
    snap_px: bool,
) {
    // 顶点坐标：逻辑点 → 物理像素（文字网格先对齐到整数物理像素）
    let snap = |q: f32| -> f32 { (q * ppp).round() / ppp };
    let sx = |i: usize| if snap_px { snap(v[i].pos.x) } else { v[i].pos.x };
    let sy = |i: usize| if snap_px { snap(v[i].pos.y) } else { v[i].pos.y };
    let p: [(f32, f32); 3] = [
        (sx(0) * ppp, sy(0) * ppp),
        (sx(1) * ppp, sy(1) * ppp),
        (sx(2) * ppp, sy(2) * ppp),
    ];
    let area = (p[1].0 - p[0].0) * (p[2].1 - p[0].1) - (p[2].0 - p[0].0) * (p[1].1 - p[0].1);
    if area.abs() < 1e-6 {
        return;
    }
    let inv = 1.0 / area;
    let x0 = p[0].0.min(p[1].0).min(p[2].0).floor().max(clip.0).max(0.0) as i32;
    let y0 = p[0].1.min(p[1].1).min(p[2].1).floor().max(clip.1).max(0.0) as i32;
    let x1 = p[0].0.max(p[1].0).max(p[2].0).ceil().min(clip.2).min(w as f32) as i32;
    let y1 = p[0].1.max(p[1].1).max(p[2].1).ceil().min(clip.3).min(h as f32) as i32;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let col = [
        v[0].color.to_array(),
        v[1].color.to_array(),
        v[2].color.to_array(),
    ];
    for py in y0..y1 {
        for px in x0..x1 {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let w0 = ((p[1].0 - x) * (p[2].1 - y) - (p[2].0 - x) * (p[1].1 - y)) * inv;
            let w1 = ((p[2].0 - x) * (p[0].1 - y) - (p[0].0 - x) * (p[2].1 - y)) * inv;
            if w0 < -1e-4 || w1 < -1e-4 {
                continue; // w2 = 1 - w0 - w1
            }
            let w2 = 1.0 - w0 - w1;
            if w2 < -1e-4 {
                continue;
            }
            let s = if let Some(t) = tex {
                // epaint 的 uv 是**归一化**的 0..1（见 tessellator 的 uv_normalizer），
                // 所以要乘回纹理尺寸才能得到纹素坐标。
                let u = v[0].uv.x * w0 + v[1].uv.x * w1 + v[2].uv.x * w2;
                let vv = v[0].uv.y * w0 + v[1].uv.y * w1 + v[2].uv.y * w2;
                sample_tex(t, u * t.w as f32, vv * t.h as f32)
            } else {
                [255.0; 4]
            };
            let idx = ((py * w + px) * 4) as usize;
            if idx + 3 >= buf.len() {
                continue;
            }
            // 预乘 over：out = src + dst * (1 - src.a)
            //
            // 注意 src 各分量都在 **0..255**（纹理字节 × 顶点色字节 / 255），
            // 所以 (1 - src.a) 之前必须先把 alpha 归一化到 0..1，
            // 否则 (1.0 - 200.0) 这种大负数会把已铺好的底色减成透明空洞
            // （表现为文字/图标在底色上"挖洞"）。
            let mut src = [0.0f32; 4];
            for k in 0..4 {
                let cv = (col[0][k] as f32 * w0 + col[1][k] as f32 * w1 + col[2][k] as f32 * w2) / 255.0;
                src[k] = s[k] * cv;
            }
            let keep = 1.0 - src[3] / 255.0;
            for k in 0..4 {
                let dst = buf[idx + k] as f32;
                buf[idx + k] = (src[k] + dst * keep).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

// ---------- 公共后端 ----------

/// 独立的字体度量通道。
///
/// egui 0.37 里 `Context::fonts` 是**懒初始化**的：`ctx.fonts` 为 `Option<Fonts>`，
/// 直到第一次 `run_ui()`（内部 `begin_pass` → `update_fonts_mut`）才被创建，
/// 在此之前调用 `ctx.fonts()` / `ctx.fonts_mut()` 会 panic
/// （"No fonts available until first call to Context::run()"）。
///
/// 但各个视图都必须**先量宽**才能算出分层窗口的物理尺寸，量宽天然发生在 pass 之前，
/// 所以这里自持一份 `epaint::Fonts`：用与 Context 完全相同的 `FontDefinitions`、
/// `TextOptions` 和 pixels_per_point，量出来的尺寸与 pass 内部的布局逐像素一致。
struct MeasureFonts {
    fonts: epaint::Fonts,
    options: epaint::TextOptions,
    ppp: f32,
    viewport: epaint::text::ViewportKey,
}

impl MeasureFonts {
    fn new(defs: egui::FontDefinitions, options: epaint::TextOptions, ppp: f32) -> Self {
        let mut fonts = epaint::Fonts::new(options, defs);
        // 固定的 viewport key：本实例只服务一个窗口，缓存不会互相踩
        let viewport = epaint::text::ViewportKey::new(0x0b17_0001);
        fonts.begin_pass(options, viewport);
        Self { fonts, options, ppp, viewport }
    }

    /// 开始"一帧"：回收上一帧没用到的 Galley 缓存（等价于 pass 内的 begin_pass）。
    fn begin_frame(&mut self, ppp: f32) {
        self.ppp = ppp;
        self.fonts.begin_pass(self.options, self.viewport);
    }

    fn layout(&mut self, text: &str, font_id: egui::FontId) -> egui::Vec2 {
        let mut view = self.fonts.with_pixels_per_point_for_viewport(self.ppp, self.viewport);
        view.layout_no_wrap(text.to_owned(), font_id, egui::Color32::WHITE).size()
    }
}

/// egui 软件后端：持有 Context 与纹理表，负责光栅化与像素块上传。
/// 每个分层窗口持有一个实例。
pub struct SoftBackend {
    ctx: egui::Context,
    /// epaint 网格的纹理（字体 / 用户上传）
    tex: HashMap<egui::TextureId, TexData>,
    /// straight RGBA 像素块的纹理缓存（key = 内容 hash）
    image_cache: HashMap<u64, egui::TextureHandle>,
    last_scale: f32,
    /// 当前目标 pixels_per_point（度量通道用，总是最新值）
    scale: f32,
    /// 字体定义（度量通道复用同一份，保证度量与渲染同源）
    font_defs: egui::FontDefinitions,
    /// pass 之外的字体度量通道（惰性创建：`Fonts::new` 很贵）
    measure: RefCell<Option<MeasureFonts>>,
    /// 上次构建字体定义时用的字体选择（切换后重建图集 / 度量通道）
    font_misans: bool,
}

impl Default for SoftBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SoftBackend {
    pub fn new() -> Self {
        let ctx = egui::Context::default();
        let font_defs = install_fonts(&ctx);
        // CentralPanel 会铺一层不透明的 panel_fill，必须压成透明（逐像素 alpha 窗口）
        ctx.style_mut_of(egui::Theme::Dark, |st| {
            st.visuals.panel_fill = egui::Color32::TRANSPARENT;
        });
        Self {
            ctx,
            tex: HashMap::new(),
            image_cache: HashMap::new(),
            last_scale: 0.0,
            scale: 1.0,
            font_defs,
            measure: RefCell::new(None),
            font_misans: settings::use_misans(),
        }
    }

    /// 取 Context（布局时读字体度量用）。
    pub fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    /// 设置 pixels_per_point（scale 变化时才真正下发）。
    ///
    /// 每个视图在画之前都会先调它，所以这里顺带推进度量通道的"帧"，
    /// 让 Galley 缓存按帧回收。
    ///
    /// 注意：**不能**用 `ctx.set_pixels_per_point` 下发！Dock / 弹窗 / 提示条共用
    /// 一个 Context 且各自的 ppp 不同（Dock 带 icon_k 因子，弹窗/提示条不带），
    /// 每次切换都会触发 egui 的 `new_zoom_factor` 路径——下一帧 egui 会无视传入的
    /// `screen_rect`，改用"上一帧 content_rect × 缩放比"，把别的视图的矩形套到
    /// 当前帧上（Dock 底部被弹窗高度裁掉 = 用户看到的"托盘消失"）。
    /// 正确做法是 `input()` 里按帧带 `native_pixels_per_point`，egui 自己算 ppp。
    pub fn set_scale(&mut self, scale: f32) {
        let scale = scale.max(0.5);
        self.scale = scale;
        self.last_scale = scale;
        // 字体设置变了（设置页切换 MiSans / 系统字体）：重建字体图集与度量通道。
        // 度量通道必须整个丢掉重来——它缓存了旧字体的 Galley 宽度，不清会错位。
        let misans = settings::use_misans();
        if misans != self.font_misans {
            self.font_misans = misans;
            self.font_defs = install_fonts(&self.ctx);
            *self.measure.get_mut() = None;
        }
        if let Some(m) = self.measure.get_mut().as_mut() {
            m.begin_frame(scale);
        }
    }

    /// 构建一帧的 RawInput：screen_rect（逻辑点）+ 本帧的 native_pixels_per_point。
    ///
    /// ppp 走 `viewports[ROOT].native_pixels_per_point`（`pixels_per_point =
    /// zoom_factor × native`），zoom_factor 恒为 1.0，egui 就永远不会走
    /// "zoom 变更 → 沿用上一帧矩形"的覆盖路径，多视图共用 Context 也安全。
    pub fn input(&self, fw: f32, fh: f32) -> egui::RawInput {
        let mut ri = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(fw, fh),
            )),
            ..Default::default()
        };
        ri.viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(self.scale);
        ri
    }

    /// 量一段文字的尺寸（**逻辑点**）。在 `run_ui` 之前调用也安全。
    pub fn text_size(&self, text: &str, font_id: egui::FontId) -> egui::Vec2 {
        let mut slot = self.measure.borrow_mut();
        let m = slot.get_or_insert_with(|| {
            let mut options = self.ctx.style_of(egui::Theme::Dark).visuals.text_options;
            // 与 InputState::default().max_texture_side 一致（RawInput 未指定时的取值）
            options.max_texture_side = 2048;
            MeasureFonts::new(self.font_defs.clone(), options, self.scale.max(0.5))
        });
        m.layout(text, font_id)
    }

    /// 量一段文字的宽度（逻辑点）。
    pub fn text_width(&self, text: &str, font_id: egui::FontId) -> f32 {
        self.text_size(text, font_id).x
    }

    /// 量一段文字的高度（逻辑点）。
    pub fn text_height(&self, text: &str, font_id: egui::FontId) -> f32 {
        self.text_size(text, font_id).y
    }

    /// 消费一帧的纹理增量（上传 / 释放）。
    ///
    /// egui 0.37 的 `TexturesDelta` 在 Drop 时会 `debug_assert` 它已被处理完，
    /// 所以这里应用完必须 `clear()`；否则 debug 版只要有字体图集增量就会 panic。
    pub fn update_textures(&mut self, delta: &mut egui::TexturesDelta) {
        for (id, deltas) in &delta.set {
            for d in deltas {
                // 字体图集是**局部更新**（pos = Some），必须合并进已有纹理，
                // 不能整体替换，否则会丢掉其它字形。
                match self.tex.get_mut(id) {
                    Some(t) => merge_delta(t, &d.image, d.pos),
                    None => {
                        if let Some(mut t) = image_to_tex(&d.image) {
                            if let Some(pos) = d.pos {
                                // 局部纹理：先占位到目标大小，再合并
                                let need_w = (pos[0] + d.image.size()[0]) as i32;
                                let need_h = (pos[1] + d.image.size()[1]) as i32;
                                let mut np = vec![0u8; (need_w * need_h * 4) as usize];
                                std::mem::swap(&mut t.px, &mut np);
                                t.w = need_w;
                                t.h = need_h;
                                merge_delta(&mut t, &d.image, d.pos);
                            }
                            self.tex.insert(*id, t);
                        }
                    }
                }
            }
        }
        for id in &delta.free {
            self.tex.remove(id);
        }
        // 告诉 epaint 这些增量已经被消费，否则 Drop 时 debug_assert 会 panic
        delta.clear();
    }

    /// 把 epaint 网格光栅化为预乘 RGBA（w*h*4）。
    pub fn rasterize(&mut self, shapes: Vec<egui::epaint::ClippedShape>, ppp: f32, w: i32, h: i32) -> Vec<u8> {
        let prims = self.ctx.tessellate(shapes, ppp);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for prim in prims {
            let Primitive::Mesh(mesh) = prim.primitive else { continue };
            let clip = (
                (prim.clip_rect.left() * ppp).floor(),
                (prim.clip_rect.top() * ppp).floor(),
                (prim.clip_rect.right() * ppp).ceil(),
                (prim.clip_rect.bottom() * ppp).ceil(),
            );
            let tex = self.tex.get(&mesh.texture_id);
            // 字体图集 = Managed(0)（epaint 约定第一张托管纹理）：文字网格做像素对齐
            let snap_px = mesh.texture_id == egui::TextureId::Managed(0);
            for tri in mesh.indices.chunks_exact(3) {
                fill_tri(
                    &mut rgba,
                    w,
                    h,
                    clip,
                    [
                        &mesh.vertices[tri[0] as usize],
                        &mesh.vertices[tri[1] as usize],
                        &mesh.vertices[tri[2] as usize],
                    ],
                    ppp,
                    tex,
                    snap_px,
                );
            }
        }
        rgba
    }

    /// 把一块 straight RGBA（w*h*4）上传为 egui 纹理，内容不变则复用。
    /// 应用图标 / 托盘图标 / 旋转封面 / 闪烁图标都走这里。
    pub fn soft_image(&mut self, rgba: &[u8], w: usize, h: usize) -> egui::TextureHandle {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in rgba {
            hash = (hash ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
        hash = (hash ^ (w as u64)).wrapping_mul(0x100_0000_01b3);
        hash = (hash ^ (h as u64)).wrapping_mul(0x100_0000_01b3);
        if let Some(handle) = self.image_cache.get(&hash) {
            return handle.clone();
        }
        let n = w * h;
        let mut pixels = Vec::with_capacity(n);
        for k in 0..n {
            let r = rgba[k * 4] as u32;
            let g = rgba[k * 4 + 1] as u32;
            let b = rgba[k * 4 + 2] as u32;
            let a = rgba[k * 4 + 3] as u32;
            pixels.push(egui::Color32::from_rgba_premultiplied(
                ((r * a) / 255) as u8,
                ((g * a) / 255) as u8,
                ((b * a) / 255) as u8,
                a as u8,
            ));
        }
        let img = egui::ColorImage {
            size: [w, h],
            source_size: egui::vec2(w as f32, h as f32),
            pixels,
        };
        let handle = self.ctx.load_texture("soft_image", img, egui::TextureOptions::LINEAR);
        self.image_cache.insert(hash, handle.clone());
        handle
    }
}

// ---------- 小工具 ----------

/// ARGB u32（0xAARRGGBB）→ egui Color32。
pub fn argb(v: u32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        ((v >> 16) & 0xFF) as u8,
        ((v >> 8) & 0xFF) as u8,
        (v & 0xFF) as u8,
        ((v >> 24) & 0xFF) as u8,
    )
}

/// 给颜色的 RGB 换一个 alpha（仍是 0xAARRGGBB）
pub fn with_alpha(c: u32, a: f32) -> u32 {
    let a = (a.clamp(0.0, 1.0) * 255.0).round() as u32;
    (a << 24) | (c & 0x00FF_FFFF)
}

/// 沿边缘的**内发光**：由外向内画几圈 alpha 递减的描边，模拟霓虹灯管的光晕。
///
/// 为什么向内：分层窗口的尺寸恰好等于形状尺寸，向外画会被窗口边裁掉；
/// 向内画反而更像"灯管边缘在发光"。圆角逐层收一点，免得内圈弧线错位。
pub fn glow_edges(p: &egui::Painter, rect: egui::Rect, cr: egui::CornerRadius, color: u32) {
    for k in 0..3 {
        let kf = k as f32;
        let d = 1.0 + kf * 1.7;
        let shrink = (kf * 1.7).round() as u8;
        let c = egui::CornerRadius {
            nw: cr.nw.saturating_sub(shrink),
            ne: cr.ne.saturating_sub(shrink),
            sw: cr.sw.saturating_sub(shrink),
            se: cr.se.saturating_sub(shrink),
        };
        p.rect_stroke(
            rect.shrink(d),
            c,
            egui::Stroke::new(1.4, argb(with_alpha(color, 0.28 - kf * 0.09))),
            egui::StrokeKind::Inside,
        );
    }
}

/// 模块图标（矢量）：`KIND_MAIL` = 信封，`KIND_CLIP` = 剪贴板。
///
/// 以前用 `✉` / `▤` 字形，雅黑和 Segoe UI Symbol 里这两个字符笔画细、
/// 基线还各不相同，15px 下基本看不清。矢量绘制能做到：
/// 线条粗细统一、光学居中、颜色跟随主题，放大缩小都清晰。
pub const KIND_MAIL: u8 = 1;
pub const KIND_CLIP: u8 = 2;

/// 在 `center` 处画一个边长为 `cell` 的模块图标（`kind` 见 `KIND_*`）
pub fn widget_icon(p: &egui::Painter, center: egui::Pos2, cell: f32, kind: u8, color: egui::Color32) {
    let stroke = egui::Stroke::new((cell * 0.085).clamp(1.1, 1.8), color);
    match kind {
        KIND_MAIL => {
            // 信封：圆角矩形 + 上方 V 形封口（比"实心信封"更轻，和细字重更配）
            let w = cell * 0.92;
            let h = cell * 0.70;
            let r = egui::Rect::from_center_size(center, egui::vec2(w, h));
            p.rect_stroke(r, egui::CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
            let cr = 2.0f32; // 与 rect_stroke 的圆角一致，线条起点内缩
            // 起始点避开圆角，线条才不从转角尖端冒出去
            let l = egui::pos2(r.left() + cr * 0.7, r.top() + 1.0);
            let rr = egui::pos2(r.right() - cr * 0.7, r.top() + 1.0);
            let mid = egui::pos2(center.x, r.top() + h * 0.58);
            p.line_segment([l, mid], stroke);
            p.line_segment([mid, rr], stroke);
        }
        KIND_CLIP => {
            // 剪贴板：板 + 顶部夹子 + 两条内容线
            let w = cell * 0.66;
            let h = cell * 0.88;
            let r = egui::Rect::from_center_size(center + egui::vec2(0.0, cell * 0.02), egui::vec2(w, h));
            p.rect_stroke(r, egui::CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
            // 夹子：一块压在顶边中央的小圆角矩形
            let cw = cell * 0.36;
            let ch = cell * 0.20;
            let clip = egui::Rect::from_min_size(
                egui::pos2(center.x - cw / 2.0, r.top() - ch * 0.45),
                egui::vec2(cw, ch),
            );
            p.rect_filled(clip, egui::CornerRadius::same(1), color);
            // 内容线：长度错开一点，避免看着像"等号"
            for (i, k) in [0.42f32, 0.62].iter().enumerate() {
                let pad = if i == 1 { w * 0.30 } else { w * 0.18 };
                let y = r.top() + h * k;
                p.line_segment(
                    [egui::pos2(r.left() + pad, y), egui::pos2(r.right() - w * 0.16, y)],
                    stroke,
                );
            }
        }
        _ => {}
    }
}

/// 玻璃底板：只铺底色 +（霓虹主题）内发光，与顶部胶囊同一画法。
/// 胶囊 / Dock / 模块条 / 音乐条共用，保证"玻璃面"视觉一致。
///
/// 注意：**不画描边**。1px 贴边描边的抗锯齿羽化会溢出窗口边界，
/// 叠到壁纸上就是"Dock 外圈一圈没裁干净的白边"（用户实测）。
/// 玻璃面的立体感靠底色本身的透明度和霓虹内发光，不再靠亮边。
pub fn glass_board(p: &egui::Painter, board: egui::Rect, cr: egui::CornerRadius) {
    p.rect_filled(board, cr, argb(cfg::pill_bg_effective()));
    if cfg::cur().glow {
        glow_edges(p, board, cr, cfg::cur().dock_edge);
    }
}

// ---------- 刘海剪影（吸顶 / 吸底共用） ----------

/// 吸顶刘海剪影的闭合路径：顶边两端各向外张开一段凹弧"耳朵"，剪影沿屏幕
/// 边缘多探出 `ew`，再用 `eh` 的深度平滑收回条身；条身底部仍是半圆。
///
/// 返回点序列**第一个点必须落在左耳与条身的交界角上**：epaint 对填充路径
/// 做"从首顶点出发"的扇形三角化，剪影对该点呈星形分布时才能正确铺满
/// （见 tessellator::triangulate_convex_path）。吸底版本由调用方把点
/// 上下镜像，绕向由填充端自动纠正。
pub fn island_silhouette(fw: f32, fh: f32, ew: f32, eh: f32, rb: f32) -> Vec<egui::Pos2> {
    // 耳朵凹弧：深度 y 处距窗口左缘 x = ew·sin(π/2 · y/eh)——
    // 贴边处张满 ew，深 eh 处收进条身，两端切线分别贴合顶边与侧边。
    let curve = |y: f32| ew * (std::f32::consts::FRAC_PI_2 * (y / eh).clamp(0.0, 1.0)).sin();
    let n = 12; // 耳朵弧采样段数
    let arc_n = 10; // 底部圆角采样段数
    let (bx0, bx1) = (ew, fw - ew); // 条身左右边（窗口比条身左右各宽一只耳朵）
    let rb = rb.min((bx1 - bx0) / 2.0).max(0.0); // 底部圆角半径
    let mut pts = Vec::with_capacity(2 * n + 2 * arc_n + 6);

    pts.push(egui::pos2(bx0, 0.0)); // 星形中心角：左耳与条身的交界
    pts.push(egui::pos2(bx1, 0.0)); // 顶边（贴屏幕边缘）
    for k in 1..=n {
        let y = eh * k as f32 / n as f32;
        pts.push(egui::pos2(fw - curve(y), y)); // 右耳凹弧
    }
    pts.push(egui::pos2(bx1, fh - rb)); // 条身右侧直边
    for k in 1..=arc_n {
        // 右下圆角（圆心 bx1-rb, fh-rb）
        let a = std::f32::consts::FRAC_PI_2 * k as f32 / arc_n as f32;
        pts.push(egui::pos2(bx1 - rb + rb * a.cos(), fh - rb + rb * a.sin()));
    }
    for k in (0..arc_n).rev() {
        // 左下圆角（圆心 bx0+rb, fh-rb）
        let a = std::f32::consts::FRAC_PI_2 * k as f32 / arc_n as f32;
        pts.push(egui::pos2(bx0 + rb - rb * a.cos(), fh - rb + rb * a.sin()));
    }
    pts.push(egui::pos2(bx0, eh)); // 条身左侧直边（回到左耳末端）
    for k in (0..n).rev() {
        let y = eh * k as f32 / n as f32;
        pts.push(egui::pos2(curve(y), y)); // 左耳凹弧，收到窗口左上角
    }
    pts
}

/// 吸顶 / 吸底剪影底板：耳朵与条身是**一整条闭合路径一次填充**——
/// 分成两块拼会在半透明底色下露出接缝（叠色比单层更实）。
/// 霓虹主题下沿剪影内缘补几圈递减的发光带，等效 `glow_edges`。
///
/// `flush_top` = 贴屏幕顶（耳朵在顶边两角）；false = 贴屏幕底（上下镜像）。
pub fn island_board(
    p: &egui::Painter, fw: f32, fh: f32, ew: f32, eh: f32, rb: f32, flush_top: bool,
) {
    let mut pts = island_silhouette(fw, fh, ew, eh, rb);
    if !flush_top {
        for pt in &mut pts {
            pt.y = fh - pt.y;
        }
    }
    let path = |fill: egui::Color32, stroke: egui::epaint::PathStroke| {
        egui::Shape::Path(egui::epaint::PathShape {
            points: pts.clone(),
            closed: true,
            fill,
            stroke,
        })
    };
    p.add(path(argb(cfg::pill_bg_effective()), egui::Stroke::NONE.into()));
    if cfg::cur().glow {
        // 内发光：外缘贴剪影、内缘逐圈收进，等价 glass_board 的 glow_edges
        let color = cfg::cur().dock_edge;
        for k in 0..3 {
            let kf = k as f32;
            let w = 2.8 + kf * 1.7;
            p.add(path(
                egui::Color32::TRANSPARENT,
                egui::epaint::PathStroke::new(w, argb(with_alpha(color, 0.28 - kf * 0.09))).inside(),
            ));
        }
    }
}

/// 分隔线（竖 1px，垂直居中）。顶部胶囊已去掉分割线，保留给其它视图备用。
#[allow(dead_code)]
pub fn divider(p: &egui::Painter, x: f32, fh: f32) {
    p.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(x, (fh - cfg::DIVIDER_H) / 2.0),
            egui::vec2(1.0, cfg::DIVIDER_H),
        ),
        0,
        argb(cfg::cur().divider),
    );
}

/// MiSans 字体文件名（随发布包放在 `<exe>/data/`，**不打进 exe**：
/// 运行时从磁盘加载，找不到就回落系统字体）。
const MISANS_FONT_FILE: &str = "MiSans-Semibold.ttf";

/// 标题/强调文字用的命名字体族名。
/// egui 的 `RichText::strong()` 只改颜色不改字重，真正要粗体必须走这个族。
pub const BOLD_FAMILY: &str = "orbit-bold";

/// 生成一段粗体文字：MiSans 模式下用 MiSans Semibold，
/// 系统字体模式下退回微软雅黑 Bold（msyhbd.ttc）。
pub fn bold(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).family(egui::FontFamily::Name(BOLD_FAMILY.into()))
}

/// 给 `Painter::text` / 度量通道用的粗体 FontId。
/// MiSans 模式下即 MiSans-Semibold；文件缺失时回落系统默认粗体字体。
pub fn bold_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(BOLD_FAMILY.into()))
}

/// 时钟/日期用的 FontId：统一走粗体族（MiSans Semibold / 系统默认粗体）。
pub fn clock_font(size: f32) -> egui::FontId {
    bold_font(size)
}

/// 依次找：发布目录 data/ → exe 同目录 → 开发期仓库 assets/fonts → 当前目录 assets/fonts。
/// 找不到 / 读失败都返回 None，调用方回落默认字体（= 系统字体效果）。
fn load_font_file(name: &str) -> Option<(Vec<u8>, std::path::PathBuf)> {
    let mut cands: Vec<std::path::PathBuf> = vec![crate::store::path(name)];
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        cands.push(dir.join(name));
        // 开发期 exe 在 target/debug，仓库根在上两级
        cands.push(dir.join("../../assets/fonts").join(name));
        cands.push(dir.join("../assets/fonts").join(name));
    }
    cands.push(std::path::PathBuf::from("assets/fonts").join(name));
    for p in cands {
        if let Ok(bytes) = std::fs::read(&p) {
            if !bytes.is_empty() {
                return Some((bytes, p));
            }
        }
    }
    None
}

/// 中文字体兜底：微软雅黑 + Segoe UI Symbol（天气/信封/剪贴板这些字形符号），
/// 追加到比例字体族末尾做回退。
///
/// 返回同一份 `FontDefinitions`：pass 之外的度量通道要复用它，
/// 否则量出来的宽度和渲染时的 Galley 不一致。
pub fn install_fonts(ctx: &egui::Context) -> egui::FontDefinitions {
    let fonts = build_font_definitions();
    ctx.set_fonts(fonts.clone());
    fonts
}

/// 构建字体定义（不设置到任何 Context）。
///
/// `settings::use_misans()` = 真 → MiSans 排在最前（中英文统一字形）；
/// = 假 → 只有系统字体（微软雅黑等），跟接入前一致。
/// 设置项变化时各窗口会重建字体图集，见 `SoftBackend::set_scale`。
pub fn build_font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let use_misans = settings::use_misans();
    static MISANS_WARNED: AtomicBool = AtomicBool::new(false);
    if use_misans {
        // MiSans 从磁盘加载（不打进 exe）：正文够清晰，标题/强调也用同一款。
        // 发布包里放在 <exe>/data/MiSans-Semibold.ttf；文件缺失时回落系统字体。
        match load_font_file(MISANS_FONT_FILE) {
            Some((bytes, _path)) => {
                fonts.font_data.insert(
                    "misans".into(),
                    std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                );
                // 插到最前面：MiSans 命中绝大多数中英文，系统字体只补它缺的字形
                for fam in fonts.families.values_mut() {
                    fam.insert(0, "misans".into());
                }
                // 粗体族：只有一个字重，仍让它指向 Semibold（比系统 Regular 稳），
                // 系统字体模式下才补 msyhbd.ttc 做真粗
                fonts.families.insert(
                    egui::FontFamily::Name(BOLD_FAMILY.into()),
                    vec!["misans".into()],
                );
                MISANS_WARNED.store(false, Ordering::Relaxed);
            }
            None => {
                if !MISANS_WARNED.swap(true, Ordering::Relaxed) {
                    crate::tlog!(
                        "[font] MiSans 未找到（{}），回落系统字体",
                        MISANS_FONT_FILE
                    );
                }
            }
        }
    }
    let candidates = [
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/msyh.ttf",
        "C:/Windows/Fonts/seguisym.ttf",
        "C:/Windows/Fonts/simhei.ttf",
    ];
    let mut loaded_cjk = Vec::new();
    for (i, path) in candidates.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(path) {
            let name = format!("soft_cjk{i}");
            fonts
                .font_data
                .insert(name.clone(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            fonts.families.entry(egui::FontFamily::Proportional).or_default().push(name.clone());
            loaded_cjk.push(name);
        }
    }
    // 粗体族的回退：MiSans 缺字形（如某些符号）时交给系统字体；
    // 系统字体模式下直接用微软雅黑 Bold，避免标题变成"假粗"（仅颜色加深）。
    let mut has_soft_bold = false;
    if !use_misans {
        if let Ok(bytes) = std::fs::read("C:/Windows/Fonts/msyhbd.ttc") {
            fonts
                .font_data
                .insert("soft_bold".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            has_soft_bold = true;
        }
    }
    let bold_family = fonts
        .families
        .entry(egui::FontFamily::Name(BOLD_FAMILY.into()))
        .or_default();
    if has_soft_bold {
        bold_family.push("soft_bold".into());
    }
    bold_family.extend(loaded_cjk);

    fonts
}
