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

use std::collections::HashMap;

use eframe::egui;
use eframe::egui::epaint::Primitive;

use crate::config as cfg;

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
pub fn fill_tri(
    buf: &mut [u8],
    w: i32,
    h: i32,
    clip: (f32, f32, f32, f32), // 物理像素 x0 y0 x1 y1
    v: [&egui::epaint::Vertex; 3],
    ppp: f32,
    tex: Option<&TexData>,
) {
    // 顶点坐标：逻辑点 → 物理像素
    let p: [(f32, f32); 3] = [
        (v[0].pos.x * ppp, v[0].pos.y * ppp),
        (v[1].pos.x * ppp, v[1].pos.y * ppp),
        (v[2].pos.x * ppp, v[2].pos.y * ppp),
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
                // epaint 的 uv 是归一化 0..1（UvRect 用 u16 定点），乘纹理尺寸得纹素坐标
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
            for k in 0..4 {
                let cv = (col[0][k] as f32 * w0 + col[1][k] as f32 * w1 + col[2][k] as f32 * w2) / 255.0;
                let src = s[k] * cv; // 纹理 × 顶点色，预乘域
                let dst = buf[idx + k] as f32;
                buf[idx + k] = (src + dst * (1.0 - src)).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

// ---------- 公共后端 ----------

/// egui 软件后端：持有 Context 与纹理表，负责光栅化与像素块上传。
/// 每个分层窗口持有一个实例。
pub struct SoftBackend {
    ctx: egui::Context,
    /// epaint 网格的纹理（字体 / 用户上传）
    tex: HashMap<egui::TextureId, TexData>,
    /// straight RGBA 像素块的纹理缓存（key = 内容 hash）
    image_cache: HashMap<u64, egui::TextureHandle>,
    last_scale: f32,
}

impl Default for SoftBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SoftBackend {
    pub fn new() -> Self {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        // CentralPanel 会铺一层不透明的 panel_fill，必须压成透明（逐像素 alpha 窗口）
        ctx.style_mut_of(egui::Theme::Dark, |st| {
            st.visuals.panel_fill = egui::Color32::TRANSPARENT;
        });
        Self {
            ctx,
            tex: HashMap::new(),
            image_cache: HashMap::new(),
            last_scale: 0.0,
        }
    }

    /// 取 Context（布局时读字体度量用）。
    pub fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    /// 设置 pixels_per_point（scale 变化时才真正下发）。
    pub fn set_scale(&mut self, scale: f32) {
        let scale = scale.max(0.5);
        if (self.last_scale - scale).abs() > f32::EPSILON {
            self.ctx.set_pixels_per_point(scale);
            self.last_scale = scale;
        }
    }

    /// 消费一帧的纹理增量（上传 / 释放）。
    pub fn update_textures(&mut self, delta: &egui::TexturesDelta) {
        for (id, deltas) in &delta.set {
            for d in deltas {
                if let Some(t) = image_to_tex(&d.image) {
                    self.tex.insert(*id, t);
                }
            }
        }
        for id in &delta.free {
            self.tex.remove(id);
        }
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

/// 分隔线（竖 1px，垂直居中）。
pub fn divider(p: &egui::Painter, x: f32, fh: f32) {
    p.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(x, (fh - cfg::DIVIDER_H) / 2.0),
            egui::vec2(1.0, cfg::DIVIDER_H),
        ),
        0,
        argb(cfg::DIVIDER_ARGB),
    );
}

/// 中文字体兜底：微软雅黑 + Segoe UI Symbol（天气/信封/剪贴板这些字形符号），
/// 追加到比例字体族末尾做回退。
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let candidates = [
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/msyh.ttf",
        "C:/Windows/Fonts/seguisym.ttf",
        "C:/Windows/Fonts/simhei.ttf",
    ];
    for (i, path) in candidates.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(format!("soft_cjk{i}"), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            fonts.families.entry(egui::FontFamily::Proportional).or_default().push(format!("soft_cjk{i}"));
        }
    }
    ctx.set_fonts(fonts);
}
