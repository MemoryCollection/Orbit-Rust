//! 顶部胶囊的 egui 软件后端（方案 C）：
//! 窗口 / 消息 / 状态仍归 `window.rs` 的主线程 wndproc，这里只负责"画"——
//! egui 立即模式布局（时钟 + widget + 闪烁图标格）→ epaint 网格 → 公共软件光栅化
//! （`egui_soft`）→ 预乘 RGBA → 交回 `render::present()`（UpdateLayeredWindow）。
//!
//! 为什么不直接用 eframe 窗口：胶囊依赖 NOACTIVATE、逐像素透明、圆角命中，
//! 且面板锚定/模块条/音乐条都挂在主线程的窗口几何上；软件后端把这些全保住。
//!
//! 交互：wndproc 把鼠标事件喂给 `push_input`（egui Pointer 事件，点坐标为
//! 逻辑点），`paint` 返回点击 / 悬停进出事件，wndproc 按旧语义分发
//! （widget 点击在按下时触发，对齐旧版手感）。

use std::time::Instant;

use eframe::egui;

use crate::config as cfg;
use crate::egui_soft::{argb, clock_font, glass_board, island_board, widget_icon, SoftBackend};
use crate::render::{ear_size, PillShape, WidgetItem};

// ---------- 对外接口 ----------

/// 单帧输入（wndproc 喂进来的鼠标事件；坐标一律**逻辑点** = 物理像素 / scale）
#[derive(Default)]
pub struct InputAcc {
    events: Vec<egui::Event>,
}

impl InputAcc {
    pub fn pointer_moved(&mut self, x: f32, y: f32) {
        self.events.push(egui::Event::PointerMoved(egui::pos2(x, y)));
    }
    pub fn pointer_button(&mut self, x: f32, y: f32, pressed: bool) {
        self.events.push(egui::Event::PointerButton {
            pos: egui::pos2(x, y),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    pub fn pointer_left(&mut self) {
        self.events.push(egui::Event::PointerGone);
    }
}

/// 一次 paint 产出的交互事件（wndproc / App 按旧语义处理）
#[derive(Debug)]
pub enum PillEvent {
    /// 按下了第 i 个 widget（按下即触发，对齐旧版）
    WidgetClick(usize),
}

/// 每帧传给 paint 的快照（数据仍由主线程 App 持有）
pub struct View<'a> {
    pub scale: f32,
    /// 合并好的时钟文本（时间 + 日期一段，`clock::text()`）
    pub clock: &'a str,
    /// 留在胶囊里的 widget（已按 module_order 排好）
    pub items: &'a [WidgetItem],
    pub shape: PillShape,
    /// 界面透明度 0..1（真正乘像素在上屏前 `render::premultiplied_rgba_to_bgra`，
    /// 这里只参与变化检测：滑条一动就得重画）
    pub alpha: f32,
}

pub struct Painted {
    /// false = 内容与上一帧完全一致，可跳过合成与上屏（纯鼠标路过的帧）
    pub changed: bool,
    pub w: i32,
    pub h: i32,
    /// 预乘 RGBA（w*h*4）
    pub rgba: Vec<u8>,
    /// 命中矩形（物理像素），与 widget 一一对应
    pub widget_rects: Vec<[f32; 4]>,
    pub events: Vec<PillEvent>,
}

// ---------- PillUi ----------

pub struct PillUi {
    soft: SoftBackend,
    input: InputAcc,
    last_hash: u64,
    t0: Instant,
}

impl Default for PillUi {
    fn default() -> Self {
        Self::new()
    }
}

impl PillUi {
    pub fn new() -> Self {
        Self {
            soft: SoftBackend::new(),
            input: InputAcc::default(),
            last_hash: 0,
            t0: Instant::now(),
        }
    }

    pub fn input(&mut self) -> &mut InputAcc {
        &mut self.input
    }

    /// 画一帧。返回 `changed=false` 时 rgba/rects 无效（内容与上帧一致）。
    pub fn paint(&mut self, v: &View) -> Painted {
        // 胶囊大小设置直接乘在"逻辑点 → 物理像素"的换算上：整块布局（字号、
        // 间距、圆角）等比放大，文字仍按目标像素重新光栅化，不会糊。
        let scale = (v.scale * crate::settings::pill_scale()).max(0.5);
        self.soft.set_scale(scale);

        // ---- 布局预量宽（走 SoftBackend 的独立度量通道：pass 之前也能量） ----
        // 时间 + 日期走时钟字体（= 粗体族）：MiSans 模式下用 MiSans Semibold，
        // 文件缺失时自动回落系统默认粗体字体。
        let time_font = clock_font(cfg::TIME_SIZE);
        let txt_font = egui::FontId::proportional(cfg::WIDGET_TEXT_SIZE);
        let icon_font = egui::FontId::proportional(cfg::WIDGET_ICON_CELL * 0.85);
        let cw = self.soft.text_width(v.clock, time_font.clone());
        let item_text_w: Vec<f32> = v
            .items
            .iter()
            .map(|it| self.soft.text_width(&it.text, txt_font.clone()))
            .collect();
        let item_w: Vec<f32> = v
            .items
            .iter()
            .zip(&item_text_w)
            .map(|(it, t)| {
                let mut w = 0.0;
                if !it.icon.is_empty() {
                    w += cfg::WIDGET_ICON_CELL;
                    if !it.text.is_empty() {
                        w += cfg::WIDGET_ICON_GAP;
                    }
                }
                w += *t;
                w
            })
            .collect();

        // ---- 总宽（与 content_width 的布局严格一致） ----
        let padx = cfg::WIDGET_PAD_X;
        let mut total = cfg::PAD_X + cw;
        if !v.items.is_empty() {
            total += cfg::GAP + 1.0 + cfg::GAP + padx;
            for (i, _) in v.items.iter().enumerate() {
                total += item_w[i] + 2.0 * padx + cfg::WIDGET_ITEM_GAP;
            }
            total -= cfg::WIDGET_ITEM_GAP;
        }
        total += cfg::PAD_X;

        // 吸顶刘海 / 吸底：贴边那条边的两角张开成"耳"（见 `render::ear_size`），
        // 窗口比条身左右各宽一只耳朵；悬浮模式窗口 = 条身，ear = 0
        let (ew, eh) = ear_size(cfg::PILL_H);
        let ear = match v.shape {
            PillShape::Float => 0.0,
            PillShape::Island => ew,
        };

        let w = ((total + 2.0 * ear) * scale).ceil() as i32;
        let h = (cfg::PILL_H * scale).ceil() as i32;
        let (fw, fh) = (w as f32 / scale, h as f32 / scale);

        // ---- egui 一帧 ----
        // ppp 必须每帧经 RawInput.viewports[ROOT].native_pixels_per_point 下发：
        // egui 0.37 的 begin_pass 对缺失值 unwrap_or(1.0) 且不跨帧持久，
        // 不带的话缩放 ≠ 1.0 时窗口按新 scale 尺寸、内容仍按 1.0 光栅化，整版错位。
        let mut input = self.soft.input(fw, fh);
        input.time = Some(self.t0.elapsed().as_secs_f64());
        input.events = std::mem::take(&mut self.input.events);

        let mut rects_pts: Vec<[f32; 4]> = Vec::new();
        let mut events: Vec<PillEvent> = Vec::new();
        let mut hov_w = None;
        // Context 内部是 Arc，clone 出来共享同一上下文（闭包里只读它）
        let ctx = self.soft.ctx().clone();

        let mut out = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let p = ui.painter().clone();
                    // 圆角半径：统一设置，不超过条高一半（14 默认 = 圆角矩形）
                    let rb = crate::settings::corner_radius_for(fh);
                    match v.shape {
                        PillShape::Float => {
                            let bg_rect = egui::Rect::from_min_size(
                                egui::pos2(0.0, 0.0),
                                egui::vec2(fw, fh),
                            );
                            glass_board(&p, bg_rect, egui::CornerRadius::same(rb.round() as u8));
                        }
                        // 吸顶刘海：耳朵 + 条身一条剪影填充（拼两块会露接缝）
                        PillShape::Island => island_board(&p, fw, fh, ew, eh, rb, true),
                    }

                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        // egui 会在每个控件后面自动加 `item_spacing`（默认 8 点），
                        // 和这里手写的间距叠在一起就会显得"图标之间空隙很大"，
                        // 而且宽度公式算不出来（宽度短一截，最右边被裁）。全部交给 add_space。
                        ui.spacing_mut().item_spacing.x = 0.0;
                        // 贴边模式的窗口比条身宽：内容整体让开耳朵，仍按条身排版
                        ui.add_space(ear);
                        ui.add_space(cfg::PAD_X);
                        let cx = ui.cursor().left();
                        p.text(
                            egui::pos2(cx, fh / 2.0),
                            egui::Align2::LEFT_CENTER,
                            v.clock,
                            time_font.clone(),
                            argb(cfg::cur().text),
                        );
                        ui.add_space(cw);

                        // ---- widget 区 ----
                        if !v.items.is_empty() {
                            // 不再画竖分割线：靠间距分隔就够，线条会打断胶囊的整体感
                            ui.add_space(cfg::GAP + 1.0 + cfg::GAP + padx);
                            for (i, item) in v.items.iter().enumerate() {
                                if i > 0 {
                                    ui.add_space(cfg::WIDGET_ITEM_GAP);
                                }
                                let hw = item_w[i] + 2.0 * padx;
                                let (rect, resp) =
                                    ui.allocate_exact_size(egui::vec2(hw, cfg::ICON_BG), egui::Sense::click());
                                rects_pts.push([rect.left(), rect.top(), rect.width(), rect.height()]);
                                if resp.hovered() {
                                    hov_w = Some(i);
                                    p.rect_filled(rect, 8.0_f32, argb(cfg::cur().hover));
                                }
                                if resp.contains_pointer()
                                    && ui.input(|inp| inp.pointer.primary_pressed())
                                {
                                    events.push(PillEvent::WidgetClick(i));
                                }
                                let mut cx = rect.left() + padx;
                                if !item.icon.is_empty() {
                                    let ic = egui::pos2(
                                        cx + cfg::WIDGET_ICON_CELL / 2.0,
                                        rect.center().y,
                                    );
                                    if item.icon_kind != 0 {
                                        widget_icon(
                                            &p,
                                            ic,
                                            cfg::WIDGET_ICON_CELL,
                                            item.icon_kind,
                                            argb(item.icon_argb),
                                        );
                                    } else {
                                        p.text(
                                            ic,
                                            egui::Align2::CENTER_CENTER,
                                            &item.icon,
                                            icon_font.clone(),
                                            argb(item.icon_argb),
                                        );
                                    }
                                    cx += cfg::WIDGET_ICON_CELL;
                                    if !item.text.is_empty() {
                                        cx += cfg::WIDGET_ICON_GAP;
                                    }
                                }
                                if !item.text.is_empty() {
                                    p.text(
                                        egui::pos2(cx, rect.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        &item.text,
                                        txt_font.clone(),
                                        argb(cfg::cur().text_body),
                                    );
                                }
                            }
                        }

                        // 托盘闪烁图标区已移除：托盘交给应用自己渲染，
                        // 胶囊宽度不随通知抖动，也就不会把整排图标挤错位
                    });
                });
        });

        // ---- 纹理增量 ----
        self.soft.update_textures(&mut out.textures_delta);

        // ---- 变化检测：内容 / 尺寸 / 悬停 / 事件，任一变了才算真要上屏 ----
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mix = |h: &mut u64, b: &[u8]| {
            for &x in b {
                *h = (*h ^ x as u64).wrapping_mul(0x100_0000_01b3);
            }
        };
        mix(&mut hash, v.clock.as_bytes());
        for (it, tw) in v.items.iter().zip(&item_text_w) {
            mix(&mut hash, it.icon.as_bytes());
            mix(&mut hash, &it.icon_argb.to_le_bytes());
            mix(&mut hash, it.text.as_bytes());
            mix(&mut hash, &tw.to_bits().to_le_bytes());
        }
        mix(&mut hash, &v.alpha.to_bits().to_le_bytes());
        // 圆角半径是全局设置：改了必须重画（否则内容哈希不变会误判成"没变"）
        mix(&mut hash, &crate::settings::corner_radius().to_bits().to_le_bytes());
        // scale 变了必须上屏：即使宽高凑巧没变，命中矩形与光栅化密度都已不同
        mix(&mut hash, &scale.to_bits().to_le_bytes());
        mix(&mut hash, &[matches!(v.shape, PillShape::Float) as u8, matches!(v.shape, PillShape::Island) as u8]);
        mix(&mut hash, &hov_w.map_or(0xffu8, |i| i as u8).to_le_bytes());
        mix(&mut hash, &w.to_le_bytes());
        mix(&mut hash, &h.to_le_bytes());
        let changed = hash != self.last_hash || !events.is_empty();
        self.last_hash = hash;

        if !changed {
            return Painted {
                changed: false,
                w,
                h,
                rgba: Vec::new(),
                widget_rects: Vec::new(),
                events,
            };
        }

        // ---- 光栅化 ----
        let rgba = self.soft.rasterize(out.shapes, out.pixels_per_point, w, h);

        let widget_rects = rects_pts
            .iter()
            .map(|r| [r[0] * scale, r[1] * scale, r[2] * scale, r[3] * scale])
            .collect();

        Painted { changed: true, w, h, rgba, widget_rects, events }
    }
}
