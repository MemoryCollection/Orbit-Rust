//! 分离模块条的 egui 软件后端绘制：圆角胶囊底 + 一排 widget（图标字 + 文本 + 悬停底）。
//! 布局与胶囊 widget 区一致，只是没有时钟、没有分隔线、没有闪烁图标。
//! egui 在**逻辑点**空间布局，`egui_soft` 光栅化成**预乘 RGBA**。

use eframe::egui;

use crate::config as cfg;
use crate::egui_soft::{argb, SoftBackend};
use crate::render::WidgetItem;

pub struct BarPainted {
    /// 预乘 RGBA（w*h*4）
    pub rgba: Vec<u8>,
    /// 每个 widget 命中矩形（物理像素 client 坐标 [x, y, w, h]）
    pub rects: Vec<[f32; 4]>,
}

/// 模块条逻辑宽度（与 paint 布局严格一致），乘 scale 得物理像素。
pub fn bar_width(widgets: &[WidgetItem]) -> f32 {
    if widgets.is_empty() {
        return 0.0;
    }
    let mut w = 2.0 * cfg::WIDGET_BAR_PAD_X;
    for (i, item) in widgets.iter().enumerate() {
        // item_w（图标格 + 间距 + 文本）
        let mut iw = 0.0;
        if !item.icon.is_empty() {
            iw += cfg::WIDGET_ICON_CELL;
            if !item.text.is_empty() {
                iw += cfg::WIDGET_ICON_GAP;
            }
        }
        w += iw;
        w += 2.0 * cfg::WIDGET_PAD_X;
        if i + 1 < widgets.len() {
            w += cfg::WIDGET_BAR_ITEM_GAP;
        }
    }
    w
}

/// 画一条模块条。w/h 是物理像素尺寸，scale 是 pixels_per_point。
pub fn paint(
    soft: &mut SoftBackend,
    w: i32, h: i32, scale: f32,
    widgets: &[WidgetItem], hovered: Option<usize>,
) -> BarPainted {
    if w <= 0 || h <= 0 || widgets.is_empty() {
        return BarPainted { rgba: Vec::new(), rects: Vec::new() };
    }
    let scale = scale.max(0.5);
    soft.set_scale(scale);
    let fw = w as f32 / scale;
    let fh = h as f32 / scale;

    let padx = cfg::WIDGET_PAD_X;
    let txt_font = egui::FontId::proportional(cfg::WIDGET_TEXT_SIZE);
    let icon_font = egui::FontId::proportional(cfg::WIDGET_ICON_CELL * 0.85);

    // 预量每个 widget 的文本宽（走 SoftBackend 的独立度量通道：pass 之前也能量）
    let item_text_w: Vec<f32> = widgets
        .iter()
        .map(|it| soft.text_width(&it.text, txt_font.clone()))
        .collect();
    let item_w: Vec<f32> = widgets
        .iter()
        .zip(&item_text_w)
        .map(|(it, tw)| {
            let mut iw = 0.0;
            if !it.icon.is_empty() {
                iw += cfg::WIDGET_ICON_CELL;
                if !it.text.is_empty() {
                    iw += cfg::WIDGET_ICON_GAP;
                }
            }
            iw += *tw;
            iw
        })
        .collect();

    let mut rects_pt: Vec<[f32; 4]> = Vec::new();
    let ctx = soft.ctx().clone();
    let out = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh))),
            ..Default::default()
        },
        |ui| {
            let p = ui.painter().clone();
            // 圆角胶囊底（半径 = 高/2）
            p.rect_filled(
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh)),
                egui::CornerRadius::same((fh / 2.0).min(255.0) as u8),
                argb(cfg::cur().pill_bg),
            );

            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add_space(cfg::WIDGET_BAR_PAD_X);
                for (i, item) in widgets.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(cfg::WIDGET_BAR_ITEM_GAP);
                    }
                    let hw = item_w[i] + 2.0 * padx;
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(hw, cfg::ICON_BG),
                        egui::Sense::hover(),
                    );
                    rects_pt.push([rect.left(), rect.top(), rect.width(), rect.height()]);
                    if Some(i) == hovered {
                        p.rect_filled(rect, 8.0_f32, argb(cfg::cur().hover));
                    }
                    let mut cx = rect.left() + padx;
                    if !item.icon.is_empty() {
                        p.text(
                            egui::pos2(cx + cfg::WIDGET_ICON_CELL / 2.0, rect.center().y),
                            egui::Align2::CENTER_CENTER,
                            &item.icon,
                            icon_font.clone(),
                            argb(item.icon_argb),
                        );
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
            });
        },
    );

    soft.update_textures(&out.textures_delta);
    let rgba = soft.rasterize(out.shapes, out.pixels_per_point, w, h);
    let rects = rects_pt
        .iter()
        .map(|r| [r[0] * scale, r[1] * scale, r[2] * scale, r[3] * scale])
        .collect();
    BarPainted { rgba, rects }
}
