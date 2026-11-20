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

/// 一个四边形，沿 a→b 方向把颜色从 `ca` 渐变到 `cb`，横向铺开 `w`。
///
/// Dock 的顶面反光和分隔线的两端淡出都靠它：epaint 的顶点色本身就是预乘 sRGBA，
/// 直接在顶点上插值就是正确的"渐变到透明"（不会出现半透明发灰）。
fn grad_quad(
    p: &egui::Painter,
    a: egui::Pos2,
    b: egui::Pos2,
    w: f32,
    ca: egui::Color32,
    cb: egui::Color32,
) {
    let dir = b - a;
    let len = dir.length();
    if len <= f32::EPSILON || w <= 0.0 {
        return;
    }
    let n = egui::vec2(-dir.y, dir.x) / len * (w * 0.5);
    let uv = egui::epaint::WHITE_UV;
    let v = |pos: egui::Pos2, color: egui::Color32| egui::epaint::Vertex { pos, uv, color };
    let mut mesh = egui::epaint::Mesh::default();
    mesh.vertices = vec![v(a - n, ca), v(a + n, ca), v(b - n, cb), v(b + n, cb)];
    mesh.indices = vec![0, 1, 2, 2, 1, 3];
    p.add(egui::Shape::mesh(mesh));
}

/// 两端淡出的分隔线（a→b 为中线，`w` 为线宽）
fn fade_line(p: &egui::Painter, a: egui::Pos2, b: egui::Pos2, w: f32, color: egui::Color32) {
    let mid = egui::pos2((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
    let clear = egui::Color32::TRANSPARENT;
    grad_quad(p, a, mid, w, clear, color);
    grad_quad(p, mid, b, w, color, clear);
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
                // ---- 底板：胶囊圆角 + 1px 内描边 + 顶面反光 ----
                let board = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh));
                let radius = cfg::DOCK_RADIUS.min(cross * 0.5);
                let cr = egui::CornerRadius::same(radius.round() as u8);
                p.rect_filled(board, cr, argb(cfg::BG_ARGB));
                p.rect_stroke(
                    board.shrink(0.5),
                    cr,
                    egui::Stroke::new(1.0, argb(cfg::DOCK_EDGE_ARGB)),
                    egui::StrokeKind::Inside,
                );
                // 迎光面的细反光：水平条画在上缘，竖排条画在左缘；两端内缩掉圆角，
                // 免得直角的亮线从圆角里探出去
                if vertical {
                    let a = egui::pos2(1.0, radius);
                    let b = egui::pos2(1.0, fh - radius);
                    grad_quad(&p, a, b, 1.0, argb(cfg::DOCK_GLOW_ARGB), argb(cfg::DOCK_GLOW_ARGB));
                } else {
                    let a = egui::pos2(radius, 1.0);
                    let b = egui::pos2(fw - radius, 1.0);
                    grad_quad(&p, a, b, 1.0, argb(cfg::DOCK_GLOW_ARGB), argb(cfg::DOCK_GLOW_ARGB));
                }

                // 沿主轴排布；首端留细长条等长占位
                let mut pos = pad + desktop_side(show_desktop);
                for (i, buf) in icons.iter().enumerate() {
                    let rc = (cross - slot) / 2.0;
                    let (rx, ry) = if vertical { (rc, pos) } else { (pos, rc) };
                    let rect = egui::Rect::from_min_size(egui::pos2(rx, ry), egui::vec2(slot, slot));
                    rects_pt.push([rx, ry, slot, slot]);
                    if Some(i) == hovered {
                        // 圆形悬停药丸：比图标再大一圈（3px），否则会被图标整个盖住，
                        // 只剩一圈弧线看不出是"底"
                        let c = rect.center();
                        let rr = slot * 0.5 + 3.0;
                        p.circle_filled(c, rr, argb(cfg::DOCK_HOVER_ARGB));
                        p.circle_stroke(
                            c,
                            rr - 0.5,
                            egui::Stroke::new(1.0, argb(cfg::DOCK_HOVER_EDGE_ARGB)),
                        );
                    }
                    if !buf.is_empty() {
                        let px = buf.len() / 4;
                        let side_px = (px as f32).sqrt().round().max(1.0) as usize;
                        let n_px = side_px * side_px;
                        if n_px * 4 <= buf.len() {
                            let tex = soft.soft_image(&buf[..n_px * 4], side_px, side_px);
                            // 悬停时图标跟着放大一点，"浮"在圆底上
                            let side = isz
                                + if Some(i) == hovered { cfg::DOCK_HOVER_GROW } else { 0.0 };
                            let ir = egui::Rect::from_min_size(
                                egui::pos2(rx + (slot - side) / 2.0, ry + (slot - side) / 2.0),
                                egui::vec2(side, side),
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
                    // 两端淡出的细分隔线（纯 1px 实线在深底上太硬）
                    let line = argb(cfg::DIVIDER_ARGB);
                    if vertical {
                        let y = pos + 0.5;
                        fade_line(
                            &p,
                            egui::pos2(dc, y),
                            egui::pos2(dc + cfg::DIVIDER_H, y),
                            1.0,
                            line,
                        );
                    } else {
                        let x = pos + 0.5;
                        fade_line(&p, egui::pos2(x, dc), egui::pos2(x, dc + cfg::DIVIDER_H), 1.0, line);
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
                            p.circle_filled(center, tslot / 2.0, argb(cfg::DOCK_HOVER_ARGB));
                            p.circle_stroke(
                                center,
                                tslot / 2.0 - 0.5,
                                egui::Stroke::new(1.0, argb(cfg::DOCK_HOVER_EDGE_ARGB)),
                            );
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
                    let bar = egui::Rect::from_min_size(egui::pos2(rx, ry), egui::vec2(bwid, bhei));
                    let r_bar = (bw / 2.0).max(1.0);
                    if hovered_desktop {
                        // 悬停：细条外面先垫一层柔和光晕，条身提亮并描边（像被点亮）
                        p.rect_filled(bar.expand(4.0), r_bar + 4.0, argb(0x1F_FF_FF_FF));
                    }
                    p.rect_filled(bar, r_bar, argb(color));
                    if hovered_desktop {
                        p.rect_stroke(
                            bar,
                            r_bar,
                            egui::Stroke::new(1.0, argb(0x59_FF_FF_FF)),
                            egui::StrokeKind::Inside,
                        );
                    }
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

        // 用 SoftBackend 的独立度量通道截断（pass 之前也能量）
        let mut s = label.to_string();
        let mut tw = self.soft.text_width(&s, font.clone());
        if tw > max_w {
            let mut chars: Vec<char> = label.chars().collect();
            while tw > max_w && chars.len() > 1 {
                chars.pop();
                s = format!("{}…", chars.iter().collect::<String>());
                tw = self.soft.text_width(&s, font.clone());
            }
        }
        let th = self.soft.text_height(&s, font.clone());

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
