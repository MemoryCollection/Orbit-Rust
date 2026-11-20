//! Dock 的 egui 软件后端绘制：圆角底板 + 应用图标槽位 + 分隔线 + 托盘图标 + 返回桌面细条，
//! 以及悬停提示条。egui 在**逻辑点**空间布局，`egui_soft` 光栅化成**预乘 RGBA**，
//! 交回 `render::present()`（UpdateLayeredWindow）上屏。
//!
//! 交互命中仍由 dock.rs 的 wndproc 按 `rects`/`tray_rects`/`desktop_rect` 自己判定
//! （这些矩形是物理像素 client 坐标），egui 这里只负责"画"。

use eframe::egui;

use crate::config as cfg;
use crate::egui_soft::{argb, SoftBackend};

/// Dock 一帧的绘制输出（与原 render::DockRender 同语义，rgba 改为预乘域）。
pub struct DockPainted {
    /// 预乘 RGBA（w*h*4）
    pub rgba: Vec<u8>,
    /// 每个应用图标槽位 [x, y, size]（物理像素 client 坐标）
    pub rects: Vec<[f32; 3]>,
    /// 每个托盘图标槽位 [x, y, size]（物理像素 client 坐标）
    pub tray_rects: Vec<[f32; 3]>,
    /// "返回桌面"细长条命中矩形 [x, y, w, h]（未显示时为 None）
    pub desktop_rect: Option<[f32; 4]>,
}

/// 右侧"返回桌面"细长条占掉的总宽度（逻辑点，含与图标区的间距）。
fn desktop_side(show_desktop: bool) -> f32 {
    if show_desktop {
        cfg::DOCK_DESKTOP_W + cfg::DOCK_DESKTOP_GAP
    } else {
        0.0
    }
}

/// Dock 逻辑尺寸（与 paint 布局严格一致）。水平返回 (宽, 高)，竖直互换。
/// 乘 scale 后即物理像素尺寸。
pub fn dock_size(
    n: usize, n_tray: usize, show_tray: bool, show_desktop: bool, vertical: bool,
) -> (f32, f32) {
    let pad = cfg::DOCK_PAD_X;
    let slot = cfg::DOCK_SLOT;
    let gap = cfg::DOCK_GAP;
    let side = desktop_side(show_desktop);
    let mut len = pad * 2.0 + side * 2.0 + n as f32 * slot + n.saturating_sub(1) as f32 * gap;
    if show_tray && n_tray > 0 {
        if n > 0 {
            len += cfg::DOCK_TRAY_SEP;
        }
        len += 1.0 + cfg::DOCK_TRAY_SEP
            + n_tray as f32 * cfg::DOCK_TRAY_SLOT
            + (n_tray as f32 - 1.0) * cfg::DOCK_TRAY_GAP;
    }
    let thick = cfg::DOCK_H;
    if vertical { (thick, len) } else { (len, thick) }
}

pub struct DockView {
    soft: SoftBackend,
}

impl Default for DockView {
    fn default() -> Self {
        Self::new()
    }
}

impl DockView {
    pub fn new() -> Self {
        Self { soft: SoftBackend::new() }
    }

    /// 画一帧 Dock。w/h 是物理像素尺寸，scale 是 pixels_per_point。
    pub fn paint(
        &mut self,
        w: i32, h: i32, scale: f32,
        icons: &[&[u8]], hovered: Option<usize>,
        tray_icons: &[&[u8]], hovered_tray: Option<usize>,
        show_tray: bool, show_desktop: bool, hovered_desktop: bool,
        vertical: bool,
    ) -> DockPainted {
        let scale = scale.max(0.5);
        self.soft.set_scale(scale);
        let fw = w as f32 / scale;
        let fh = h as f32 / scale;

        let pad = cfg::DOCK_PAD_X;
        let slot = cfg::DOCK_SLOT;
        let gap = cfg::DOCK_GAP;
        let isz = cfg::DOCK_ICON;
        let cross = if vertical { fw } else { fh };
        let long = if vertical { fh } else { fw };

        let mut rects_pt: Vec<[f32; 4]> = Vec::new();
        let mut tray_rects_pt: Vec<[f32; 4]> = Vec::new();
        let mut desktop_rect_pt: Option<[f32; 4]> = None;

        let ctx = self.soft.ctx().clone();
        let soft = &mut self.soft;
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(fw, fh),
                )),
                ..Default::default()
            },
            |ui| {
                let p = ui.painter().clone();
                // 圆角底板
                p.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh)),
                    egui::CornerRadius::same(cfg::DOCK_RADIUS as u8),
                    argb(cfg::BG_ARGB),
                );

                // 沿主轴排布；首端留细长条等长占位
                let mut pos = pad + desktop_side(show_desktop);
                for (i, buf) in icons.iter().enumerate() {
                    let rc = (cross - slot) / 2.0;
                    let (rx, ry) = if vertical { (rc, pos) } else { (pos, rc) };
                    let rect = egui::Rect::from_min_size(egui::pos2(rx, ry), egui::vec2(slot, slot));
                    rects_pt.push([rx, ry, slot, slot]);
                    if Some(i) == hovered {
                        p.rect_filled(rect, 12.0_f32, argb(cfg::HOVER_ARGB));
                    }
                    if !buf.is_empty() {
                        let px = buf.len() / 4;
                        let side_px = (px as f32).sqrt().round().max(1.0) as usize;
                        let n_px = side_px * side_px;
                        if n_px * 4 <= buf.len() {
                            let tex = soft.soft_image(&buf[..n_px * 4], side_px, side_px);
                            let ir = egui::Rect::from_min_size(
                                egui::pos2(rx + (slot - isz) / 2.0, ry + (slot - isz) / 2.0),
                                egui::vec2(isz, isz),
                            );
                            p.image(
                                tex.id(),
                                ir,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                egui::Color32::WHITE,
                            );
                        }
                    }
                    pos += slot + gap;
                }
                if !icons.is_empty() {
                    pos -= gap;
                }

                // 分隔线 + 托盘图标
                if show_tray && !tray_icons.is_empty() {
                    if !icons.is_empty() {
                        pos += cfg::DOCK_TRAY_SEP;
                    }
                    let dc = (cross - cfg::DIVIDER_H) / 2.0;
                    if vertical {
                        p.rect_filled(
                            egui::Rect::from_min_size(egui::pos2(dc, pos), egui::vec2(cfg::DIVIDER_H, 1.0)),
                            0.0_f32,
                            argb(cfg::DIVIDER_ARGB),
                        );
                    } else {
                        p.rect_filled(
                            egui::Rect::from_min_size(egui::pos2(pos, dc), egui::vec2(1.0, cfg::DIVIDER_H)),
                            0.0_f32,
                            argb(cfg::DIVIDER_ARGB),
                        );
                    }
                    pos += 1.0 + cfg::DOCK_TRAY_SEP;

                    let tslot = cfg::DOCK_TRAY_SLOT;
                    let tisz = cfg::DOCK_TRAY_ICON;
                    for (i, buf) in tray_icons.iter().enumerate() {
                        let rc = (cross - tslot) / 2.0;
                        let (rx, ry) = if vertical { (rc, pos) } else { (pos, rc) };
                        tray_rects_pt.push([rx, ry, tslot, tslot]);
                        if Some(i) == hovered_tray {
                            let center = egui::pos2(rx + tslot / 2.0, ry + tslot / 2.0);
                            p.circle_filled(center, tslot / 2.0, argb(cfg::HOVER_ARGB));
                        }
                        if !buf.is_empty() {
                            let px = buf.len() / 4;
                            let side_px = (px as f32).sqrt().round().max(1.0) as usize;
                            let n_px = side_px * side_px;
                            if n_px * 4 <= buf.len() {
                                let tex = soft.soft_image(&buf[..n_px * 4], side_px, side_px);
                                let ir = egui::Rect::from_min_size(
                                    egui::pos2(rx + (tslot - tisz) / 2.0, ry + (tslot - tisz) / 2.0),
                                    egui::vec2(tisz, tisz),
                                );
                                p.image(
                                    tex.id(),
                                    ir,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    egui::Color32::WHITE,
                                );
                            }
                        }
                        pos += tslot + cfg::DOCK_TRAY_GAP;
                    }
                }

                // 末端"返回桌面"细长条
                if show_desktop {
                    let bw = cfg::DOCK_DESKTOP_W;
                    let bl = long * cfg::DOCK_DESKTOP_H;
                    let (bx, by, bwid, bhei) = if vertical {
                        (0.0, long - pad - bl, bl, bw)
                    } else {
                        (long - pad - bw, 0.0, bw, bl)
                    };
                    let (rx, ry) = if vertical {
                        ((cross - bwid) / 2.0, by)
                    } else {
                        (bx, (cross - bhei) / 2.0)
                    };
                    let color = if hovered_desktop {
                        cfg::DOCK_DESKTOP_HOVER_ARGB
                    } else {
                        cfg::DOCK_DESKTOP_ARGB
                    };
                    p.rect_filled(
                        egui::Rect::from_min_size(egui::pos2(rx, ry), egui::vec2(bwid, bhei)),
                        egui::CornerRadius::same((bw / 2.0).max(1.0) as u8),
                        argb(color),
                    );
                    desktop_rect_pt = Some(if vertical {
                        [0.0, ry - cfg::DOCK_DESKTOP_GAP, cross, bhei + cfg::DOCK_DESKTOP_GAP]
                    } else {
                        [rx - cfg::DOCK_DESKTOP_GAP, 0.0, bwid + cfg::DOCK_DESKTOP_GAP, cross]
                    });
                }
            },
        );

        self.soft.update_textures(&out.textures_delta);
        let rgba = self.soft.rasterize(out.shapes, out.pixels_per_point, w, h);

        // 命中矩形：逻辑点 → 物理像素
        let rects = rects_pt
            .iter()
            .map(|r| [r[0] * scale, r[1] * scale, r[2] * scale])
            .collect();
        let tray_rects = tray_rects_pt
            .iter()
            .map(|r| [r[0] * scale, r[1] * scale, r[2] * scale])
            .collect();
        let desktop_rect = desktop_rect_pt.map(|r| [r[0] * scale, r[1] * scale, r[2] * scale, r[3] * scale]);

        DockPainted { rgba, rects, tray_rects, desktop_rect }
    }

    /// 画悬停提示条。返回 (w, h, 预乘 RGBA)。文本过长按 TIP_MAX_W 截断加省略号。
    pub fn paint_tooltip(&mut self, text: &str, scale: f32) -> (i32, i32, Vec<u8>) {
        let label = text.trim();
        if label.is_empty() {
            return (0, 0, Vec::new());
        }
        let scale = scale.max(0.5);
        self.soft.set_scale(scale);
        let font = egui::FontId::proportional(cfg::TIP_TEXT_SIZE);
        let max_w = cfg::TIP_MAX_W;

        // 用 egui 字体度量截断
        let mut s = label.to_string();
        let mut tw = self.soft.ctx().fonts_mut(|f| {
            f.layout_no_wrap(s.clone(), font.clone(), egui::Color32::WHITE).size().x
        });
        if tw > max_w {
            let mut chars: Vec<char> = label.chars().collect();
            while tw > max_w && chars.len() > 1 {
                chars.pop();
                s = format!("{}…", chars.iter().collect::<String>());
                tw = self.soft.ctx().fonts_mut(|f| {
                    f.layout_no_wrap(s.clone(), font.clone(), egui::Color32::WHITE).size().x
                });
            }
        }
        let th = self.soft.ctx().fonts_mut(|f| {
            f.layout_no_wrap(s.clone(), font.clone(), egui::Color32::WHITE).size().y
        });

        let pad_x = cfg::TIP_PAD_X;
        let pad_y = cfg::TIP_PAD_Y;
        let fw = tw + pad_x * 2.0;
        let fh = th + pad_y * 2.0;
        let w = (fw * scale).ceil() as i32;
        let h = (fh * scale).ceil() as i32;
        if w <= 0 || h <= 0 {
            return (0, 0, Vec::new());
        }

        let ctx = self.soft.ctx().clone();
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(fw, fh),
                )),
                ..Default::default()
            },
            |ui| {
                let p = ui.painter().clone();
                p.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(0.5, 0.5), egui::vec2(fw - 1.0, fh - 1.0)),
                    egui::CornerRadius::same(cfg::TIP_RADIUS as u8),
                    argb(cfg::TIP_BG_ARGB),
                );
                p.text(
                    egui::pos2(pad_x, pad_y),
                    egui::Align2::LEFT_TOP,
                    &s,
                    font.clone(),
                    argb(cfg::TIP_TEXT_ARGB),
                );
            },
        );
        self.soft.update_textures(&out.textures_delta);
        let rgba = self.soft.rasterize(out.shapes, out.pixels_per_point, w, h);
        (w, h, rgba)
    }
}
