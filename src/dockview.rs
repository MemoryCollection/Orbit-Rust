//! Dock 的 egui 软件后端绘制：圆角底板 + 应用图标槽位 + 分隔线 + 托盘图标 + 返回桌面细条，
//! 以及悬停提示条。egui 在**逻辑点**空间布局，`egui_soft` 光栅化成**预乘 RGBA**，
//! 交回 `render::present()`（UpdateLayeredWindow）上屏。
//!
//! 交互命中仍由 dock.rs 的 wndproc 按 `rects`/`tray_rects`/`desktop_rect` 自己判定
//! （这些矩形是物理像素 client 坐标），egui 这里只负责"画"。

use eframe::egui;

use crate::config as cfg;
use crate::egui_soft::{argb, glow_edges, SoftBackend};

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

/// 右侧"返回桌面"按钮占掉的总宽度（逻辑点，含与图标区的间距）。
fn desktop_side(show_desktop: bool) -> f32 {
    if show_desktop {
        cfg::DOCK_DESKTOP_W + cfg::DOCK_DESKTOP_GAP
    } else {
        0.0
    }
}

/// Dock 图标大小设置（1.0 = 基准）：整个 Dock 等比缩放
fn icon_k() -> f32 {
    crate::settings::icon_scale().clamp(0.5, 2.0)
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
        // 任务组与托盘组之间也只隔一个普通间距（分隔线画在间隙里，不额外占宽）
        if n > 0 {
            len += gap;
        }
        len += n_tray as f32 * cfg::DOCK_TRAY_SLOT + (n_tray as f32 - 1.0) * cfg::DOCK_TRAY_GAP;
    }
    // 整套尺寸按"图标大小"设置等比放大/缩小（paint 里用同一个系数光栅化）
    let k = icon_k();
    let thick = cfg::DOCK_H * k;
    let len = len * k;
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
        icons: &[&[u8]], hovered: Option<usize>, active: Option<usize>,
        tray_icons: &[&[u8]], hovered_tray: Option<usize>,
        show_tray: bool, show_desktop: bool, hovered_desktop: bool,
        vertical: bool,
    ) -> DockPainted {
        // Dock 图标大小设置：整套尺寸（含底板高度、槽位、间距）等比缩放，
        // 文字/图标按新的目标像素重新光栅化，不是位图拉伸
        let scale = (scale * icon_k()).max(0.5);
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
                let radius = cfg::cur().dock_radius.min(cross * 0.5);
                let cr = egui::CornerRadius::same(radius.round() as u8);
                p.rect_filled(board, cr, argb(cfg::pill_bg_effective()));
                p.rect_stroke(
                    board.shrink(0.5),
                    cr,
                    egui::Stroke::new(1.0, argb(cfg::cur().dock_edge)),
                    egui::StrokeKind::Inside,
                );
                // 霓虹主题：再沿边缘叠一圈内发光
                if cfg::cur().glow {
                    glow_edges(&p, board, cr, cfg::cur().dock_edge);
                }
                // 迎光面的细反光：水平条画在上缘，竖排条画在左缘；两端内缩掉圆角，
                // 免得直角的亮线从圆角里探出去
                if vertical {
                    let a = egui::pos2(1.0, radius);
                    let b = egui::pos2(1.0, fh - radius);
                    grad_quad(&p, a, b, 1.0, argb(cfg::cur().dock_glow), argb(cfg::cur().dock_glow));
                } else {
                    let a = egui::pos2(radius, 1.0);
                    let b = egui::pos2(fw - radius, 1.0);
                    grad_quad(&p, a, b, 1.0, argb(cfg::cur().dock_glow), argb(cfg::cur().dock_glow));
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
                        p.circle_filled(c, rr, argb(cfg::cur().dock_hover));
                        p.circle_stroke(
                            c,
                            rr - 0.5,
                            egui::Stroke::new(1.0, argb(cfg::cur().dock_hover_edge)),
                        );
                    }
                    // 浅色主题下彩色图标直接压在浅底上会发脏，先垫一层圆角白底
                    if !buf.is_empty() && cfg::cur().icon_backing != 0 {
                        let br = egui::Rect::from_center_size(
                            rect.center(),
                            egui::vec2(isz + 2.0, isz + 2.0),
                        );
                        p.rect_filled(
                            br,
                            egui::CornerRadius::same((isz * 0.22) as u8),
                            argb(cfg::cur().icon_backing),
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
                    // 前台应用：图标下沿一条短横条（任务栏那套视觉语言）。
                    // 竖排时整条转向，贴在"远离屏幕中心"的那一侧
                    if Some(i) == active {
                        let th = cfg::DOCK_ACTIVE_H;
                        let tw = (slot * 0.34).max(9.0);
                        let bar = if vertical {
                            egui::Rect::from_min_size(
                                egui::pos2(cross - th - 1.6, rect.center().y - tw / 2.0),
                                egui::vec2(th, tw),
                            )
                        } else {
                            egui::Rect::from_min_size(
                                egui::pos2(rect.center().x - tw / 2.0, cross - th - 1.6),
                                egui::vec2(tw, th),
                            )
                        };
                        p.rect_filled(bar, th / 2.0, argb(cfg::cur().accent));
                    }

                    pos += slot + gap;
                }
                if !icons.is_empty() {
                    pos -= gap;
                }

                // 分隔线 + 托盘图标
                if show_tray && !tray_icons.is_empty() {
                    // 任务图标组与托盘图标组之间**只隔一个普通间距**：分隔线画在这格
                    // 间隙正中（不再额外加 DOCK_TRAY_SEP），整排图标的间距保持一致
                    if !icons.is_empty() {
                        let dc = (cross - cfg::DIVIDER_H) / 2.0;
                        let mid = pos + gap * 0.5;
                        // 两端淡出的细分隔线（纯 1px 实线在深底上太硬）
                        let line = argb(cfg::cur().divider);
                        if vertical {
                            fade_line(
                                &p,
                                egui::pos2(dc, mid),
                                egui::pos2(dc + cfg::DIVIDER_H, mid),
                                1.0,
                                line,
                            );
                        } else {
                            fade_line(
                                &p,
                                egui::pos2(mid, dc),
                                egui::pos2(mid, dc + cfg::DIVIDER_H),
                                1.0,
                                line,
                            );
                        }
                        pos += gap;
                    }

                    let tslot = cfg::DOCK_TRAY_SLOT;
                    let tisz = cfg::DOCK_TRAY_ICON;
                    for (i, buf) in tray_icons.iter().enumerate() {
                        let rc = (cross - tslot) / 2.0;
                        let (rx, ry) = if vertical { (rc, pos) } else { (pos, rc) };
                        tray_rects_pt.push([rx, ry, tslot, tslot]);
                        if Some(i) == hovered_tray {
                            // 与任务图标同一套悬停药丸（半径同样 +3），槽位一样大才对得齐
                            let center = egui::pos2(rx + tslot / 2.0, ry + tslot / 2.0);
                            let rr = tslot * 0.5 + 3.0;
                            p.circle_filled(center, rr, argb(cfg::cur().dock_hover));
                            p.circle_stroke(
                                center,
                                rr - 0.5,
                                egui::Stroke::new(1.0, argb(cfg::cur().dock_hover_edge)),
                            );
                        }
                        if !buf.is_empty() && cfg::cur().icon_backing != 0 {
                            let bc = egui::pos2(rx + tslot / 2.0, ry + tslot / 2.0);
                            p.rect_filled(
                                egui::Rect::from_center_size(
                                    bc,
                                    egui::vec2(tisz + 2.0, tisz + 2.0),
                                ),
                                egui::CornerRadius::same((tisz * 0.22) as u8),
                                argb(cfg::cur().icon_backing),
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

                // 末端"返回桌面"细竖条：一根短圆角条，不做图标按钮（克制、不抢视觉）
                if show_desktop {
                    // 条的"厚度"永远垂直于 Dock 主轴（横排 → 竖条，竖排 → 横条），
                    // 条的长度横跨 Dock 厚度方向（`DOCK_DESKTOP_H` 占位，留上下呼吸）
                    let thin = cfg::DOCK_DESKTOP_W.min(cross - 12.0).max(2.0);
                    let span = (cross * cfg::DOCK_DESKTOP_H).max(8.0);
                    let (bw, bh) = if vertical { (span, thin) } else { (thin, span) };
                    let (bx, by) = if vertical {
                        (0.0, long - pad - bh)
                    } else {
                        (long - pad - bw, 0.0)
                    };
                    let (rx, ry) = if vertical {
                        ((cross - bw) / 2.0, by)
                    } else {
                        (bx, (cross - bh) / 2.0)
                    };
                    let bar = egui::Rect::from_min_size(egui::pos2(rx, ry), egui::vec2(bw, bh));
                    let r_bar = thin / 2.0;

                    // **只有鼠标停在这一条上才画出形状**（用户要求"不触发就不显示"）：
                    // 平时整条透明，只保留命中区，悬停时条身变强调色 + 外放一圈柔光
                    // （向内画，窗口边不会裁掉）
                    if hovered_desktop {
                        let text = cfg::cur().text;
                        p.rect_filled(
                            bar.expand(2.5),
                            r_bar + 2.5,
                            argb(crate::egui_soft::with_alpha(text, 0.10)),
                        );
                        p.rect_filled(
                            bar,
                            r_bar,
                            argb(crate::egui_soft::with_alpha(cfg::cur().accent, 0.95)),
                        );
                    }

                    // 命中区要比细条宽一圈，否则 7 点宽根本点不准；两侧都是空白，不会抢图标
                    let gap = cfg::DOCK_DESKTOP_GAP;
                    desktop_rect_pt = Some(if vertical {
                        [0.0, ry - gap, cross, bh + gap * 2.0]
                    } else {
                        [rx - gap, 0.0, bw + gap * 2.0, cross]
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
                    egui::CornerRadius::same(cfg::cur().tip_radius as u8),
                    argb(cfg::cur().tip_bg),
                );
                p.text(
                    egui::pos2(pad_x, pad_y),
                    egui::Align2::LEFT_TOP,
                    &s,
                    font.clone(),
                    argb(cfg::cur().tip_text),
                );
            },
        );
        self.soft.update_textures(&out.textures_delta);
        let rgba = self.soft.rasterize(out.shapes, out.pixels_per_point, w, h);
        (w, h, rgba)
    }
}
