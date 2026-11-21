//! Dock 的 egui 软件后端绘制：圆角底板 + 应用图标槽位 + 分隔线 + 托盘图标 + 返回桌面细条，
//! 以及悬停提示条。egui 在**逻辑点**空间布局，`egui_soft` 光栅化成**预乘 RGBA**，
//! 交回 `render::present()`（UpdateLayeredWindow）上屏。
//!
//! 交互命中仍由 dock.rs 的 wndproc 按 `rects`/`tray_rects`/`desktop_rect` 自己判定
//! （这些矩形是物理像素 client 坐标），egui 这里只负责"画"。

use eframe::egui;

use crate::config as cfg;
use crate::egui_soft::{argb, glass_board_with, SoftBackend};

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

/// 波浪放大倍数：光标停在主轴 `c` 处时，主轴坐标 `axis` 上的图标放大多少。
/// 高斯衰减（macOS Dock 同款手感）：光标正上方 ≈ 1+AMP，相邻格平滑过渡到 1。
/// `cursor_axis = None`（光标不在 Dock 上）时恒为 1。
fn wave_mag(cursor_axis: Option<f32>, axis: f32) -> f32 {
    match cursor_axis {
        None => 1.0,
        Some(c) => {
            let d = (c - axis) / cfg::DOCK_WAVE_SIGMA;
            1.0 + cfg::DOCK_WAVE_AMP * (-d * d).exp()
        }
    }
}

/// 窗口在内侧多留的一圈**透明留白**（逻辑点，k=1 基准）。
///
/// 悬停的图标会被抬到"一半在 Dock 内、一半在 Dock 外"（中心压到 Dock 内缘），
/// 伸出去的半截要画在 Dock 底板之外——分层窗口恰好等于底板尺寸就会把它裁掉。
/// 窗口在内侧加一圈透明留白（悬停满幅时图标最远伸出 `DOCK_ICON*(1+AMP)/2`），
/// 物理像素 = 本值 × 图标倍率 × DPI。
pub fn dock_margin() -> f32 {
    (cfg::DOCK_ICON * (1.0 + cfg::DOCK_WAVE_AMP) * 0.5).ceil() + 2.0
}

/// 前台指示条：图标下沿一条短横条（任务栏那套视觉语言）。竖排时整条转向，
/// 贴在"远离屏幕中心"的那一侧。任务图标和托盘图标共用，前台是谁就给谁画。
/// `rect`/`cross` 是底板坐标，`(ox, oy)` 把结果平移进窗口（底板原点）。
fn active_bar(
    p: &egui::Painter, rect: egui::Rect, slot: f32, cross: f32, vertical: bool,
    ox: f32, oy: f32,
) {
    let th = cfg::DOCK_ACTIVE_H;
    let tw = (slot * 0.34).max(9.0);
    let bar = if vertical {
        egui::Rect::from_min_size(
            egui::pos2(cross - th - 1.6 + ox, rect.center().y - tw / 2.0 + oy),
            egui::vec2(th, tw),
        )
    } else {
        egui::Rect::from_min_size(
            egui::pos2(rect.center().x - tw / 2.0 + ox, cross - th - 1.6 + oy),
            egui::vec2(tw, th),
        )
    };
    p.rect_filled(bar, th / 2.0, argb(cfg::cur().accent));
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
    ///
    /// `cursor`：光标在 Dock 窗口内的位置（逻辑点），用于波浪放大；
    /// None = 光标不在 Dock 上，全部图标按基准大小画。
    pub fn paint(
        &mut self,
        w: i32, h: i32, scale: f32,
        icons: &[&[u8]], active: Option<usize>,
        tray_icons: &[&[u8]], active_tray: Option<usize>,
        show_tray: bool, show_desktop: bool, hovered_desktop: bool,
        vertical: bool,
        cursor: Option<(f32, f32)>,
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

        // ---- 波浪放大 ----
        // 光标沿主轴的坐标；图标按与光标的距离高斯放大（覆盖相邻 2~3 格）
        // 放大时图标朝屏幕内侧"浮出"的方向：底部 Dock 向上、左锚向右、右锚向左
        let (gx, gy) = match crate::settings::dock_anchor() {
            crate::settings::DockAnchor::Bottom => (0.0, -1.0),
            crate::settings::DockAnchor::Top => (0.0, 1.0),
            crate::settings::DockAnchor::Left => (1.0, 0.0),
            crate::settings::DockAnchor::Right => (-1.0, 0.0),
        };
        // 窗口在内侧多一圈透明留白，装下"伸出去"的半截图标。
        // 底板原点 (ox, oy)：内侧在左/上时，底板被推向留白的另一侧。
        let m = dock_margin();
        let ox = if gx < 0.0 { m } else { 0.0 };
        let oy = if gy < 0.0 { m } else { 0.0 };
        // 底板尺寸（窗口 = 底板 + 内侧留白）
        let bw = if gx != 0.0 { fw - m } else { fw };
        let bh = if gy != 0.0 { fh - m } else { fh };
        let cross = if vertical { bw } else { bh };
        let long = if vertical { bh } else { bw };
        let cursor_axis = cursor.map(|(x, y)| {
            let (x, y) = (x - ox, y - oy);
            if vertical { y } else { x }
        });

        let mut rects_pt: Vec<[f32; 4]> = Vec::new();
        let mut tray_rects_pt: Vec<[f32; 4]> = Vec::new();
        let mut desktop_rect_pt: Option<[f32; 4]> = None;

        let ctx = self.soft.ctx().clone();
        let soft = &mut self.soft;
        // ppp 每帧走 RawInput（见 egui_soft::input 的注释）：不带会被 egui 当 1.0，
        // 缩放 ≠ 1.0 时底板/图标/命中矩形整体错位
        let input = soft.input(fw, fh);
        let mut out = ctx.run_ui(
            input,
            |ui| {
                let p = ui.painter().clone();
                // ---- 底板：圆角 + 内发光 ----
                let board = egui::Rect::from_min_size(egui::pos2(ox, oy), egui::vec2(bw, bh));
                // 圆角走统一设置（默认圆角矩形）；主题的 dock_radius 作上限
                let radius = crate::settings::corner_radius()
                    .min(cfg::cur().dock_radius)
                    .min(cross * 0.5)
                    .max(0.0);
                let cr = egui::CornerRadius::same(radius.round() as u8);
                // 底色 + 内描边（描边内移到 shrink(1.5)，羽化不出窗——
                // 画在 shrink(0.5) 时会往壁纸溢一圈白边）
                glass_board_with(&p, board, cr, cfg::dock_bg_effective());
                // 迎光面的细反光：水平条画在上缘，竖排条画在左缘；两端内缩掉圆角，
                // 免得直角的亮线从圆角里探出去
                if vertical {
                    let a = egui::pos2(ox + 1.0, oy + radius);
                    let b = egui::pos2(ox + 1.0, oy + bh - radius);
                    grad_quad(&p, a, b, 1.0, argb(cfg::cur().dock_glow), argb(cfg::cur().dock_glow));
                } else {
                    let a = egui::pos2(ox + radius, oy + 1.0);
                    let b = egui::pos2(ox + bw - radius, oy + 1.0);
                    grad_quad(&p, a, b, 1.0, argb(cfg::cur().dock_glow), argb(cfg::cur().dock_glow));
                }

                // 沿主轴排布；首端留细长条等长占位
                let mut pos = pad + desktop_side(show_desktop);
                for (i, buf) in icons.iter().enumerate() {
                    let rc = (cross - slot) / 2.0;
                    let (rx, ry) = if vertical { (rc, pos) } else { (pos, rc) };
                    let rect = egui::Rect::from_min_size(egui::pos2(rx, ry), egui::vec2(slot, slot));
                    rects_pt.push([rx, ry, slot, slot]);
                    // 波浪放大：中心沿主轴按槽位算，放大倍数随光标距离衰减。
                    // 满幅悬停时图标中心被抬到 Dock 内缘（rise = cross/2）——
                    // 一半留在 Dock 里、一半伸进外侧留白（macOS 手感）；
                    // 相邻图标按同一比例平滑过渡，光标不在时 rise = 0。
                    let axis = pos + slot * 0.5;
                    let mag = wave_mag(cursor_axis, axis);
                    let side = isz * mag;
                    let rise = (cross * 0.5) * (mag - 1.0) / cfg::DOCK_WAVE_AMP;
                    let icon_c = egui::pos2(
                        ox + (rx + slot * 0.5) + gx * rise,
                        oy + (ry + slot * 0.5) + gy * rise,
                    );
                    // 浅色主题下彩色图标直接压在浅底上会发脏，先垫一层圆角白底
                    if !buf.is_empty() && cfg::cur().icon_backing != 0 {
                        p.rect_filled(
                            egui::Rect::from_center_size(
                                icon_c,
                                egui::vec2(side + 2.0, side + 2.0),
                            ),
                            egui::CornerRadius::same((side * 0.22) as u8),
                            argb(cfg::cur().icon_backing),
                        );
                    }
                    if !buf.is_empty() {
                        let px = buf.len() / 4;
                        let side_px = (px as f32).sqrt().round().max(1.0) as usize;
                        let n_px = side_px * side_px;
                        if n_px * 4 <= buf.len() {
                            let tex = soft.soft_image(&buf[..n_px * 4], side_px, side_px);
                            p.image(
                                tex.id(),
                                egui::Rect::from_center_size(icon_c, egui::vec2(side, side)),
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                egui::Color32::WHITE,
                            );
                        }
                    }
                    // 前台应用：图标下沿一条短横条（任务栏那套视觉语言）。
                    // 竖排时整条转向，贴在"远离屏幕中心"的那一侧
                    if Some(i) == active {
                        active_bar(&p, rect, slot, cross, vertical, ox, oy);
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
                                egui::pos2(dc + ox, mid + oy),
                                egui::pos2(dc + cfg::DIVIDER_H + ox, mid + oy),
                                1.0,
                                line,
                            );
                        } else {
                            fade_line(
                                &p,
                                egui::pos2(mid + ox, dc + oy),
                                egui::pos2(mid + ox, dc + cfg::DIVIDER_H + oy),
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
                        // 托盘图标与任务图标同一套波浪放大（大小已统一）
                        let t_axis = pos + tslot * 0.5;
                        let t_mag = wave_mag(cursor_axis, t_axis);
                        let t_side = tisz * t_mag;
                        let t_rise = (cross * 0.5) * (t_mag - 1.0) / cfg::DOCK_WAVE_AMP;
                        let t_center = egui::pos2(
                            ox + (rx + tslot * 0.5) + gx * t_rise,
                            oy + (ry + tslot * 0.5) + gy * t_rise,
                        );
                        if !buf.is_empty() && cfg::cur().icon_backing != 0 {
                            p.rect_filled(
                                egui::Rect::from_center_size(
                                    t_center,
                                    egui::vec2(t_side + 2.0, t_side + 2.0),
                                ),
                                egui::CornerRadius::same((t_side * 0.22) as u8),
                                argb(cfg::cur().icon_backing),
                            );
                        }
                        if !buf.is_empty() {
                            let px = buf.len() / 4;
                            let side_px = (px as f32).sqrt().round().max(1.0) as usize;
                            let n_px = side_px * side_px;
                            if n_px * 4 <= buf.len() {
                                let tex = soft.soft_image(&buf[..n_px * 4], side_px, side_px);
                                p.image(
                                    tex.id(),
                                    egui::Rect::from_center_size(
                                        t_center,
                                        egui::vec2(t_side, t_side),
                                    ),
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    egui::Color32::WHITE,
                                );
                            }
                        }
                        // 前台窗口就是这个托盘程序时，图标下沿同样给一条指示条
                        // （之前只有任务图标有，托盘在最前就没有视觉反馈）
                        if Some(i) == active_tray {
                            let trect =
                                egui::Rect::from_min_size(egui::pos2(rx, ry), egui::vec2(tslot, tslot));
                            active_bar(&p, trect, tslot, cross, vertical, ox, oy);
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
                    let (bw2, bh2) = if vertical { (span, thin) } else { (thin, span) };
                    let (bx2, by2) = if vertical {
                        (0.0, long - pad - bh2)
                    } else {
                        (long - pad - bw2, 0.0)
                    };
                    let (rx, ry) = if vertical {
                        ((cross - bw2) / 2.0, by2)
                    } else {
                        (bx2, (cross - bh2) / 2.0)
                    };
                    let bar = egui::Rect::from_min_size(egui::pos2(rx + ox, ry + oy), egui::vec2(bw2, bh2));
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
                        [0.0, ry - gap, cross, thin + gap * 2.0]
                    } else {
                        [rx - gap, 0.0, thin + gap * 2.0, cross]
                    });
                }
            },
        );

        self.soft.update_textures(&mut out.textures_delta);
        let rgba = self.soft.rasterize(out.shapes, out.pixels_per_point, w, h);

        // 命中矩形：底板坐标 + 底板原点 → 物理像素窗口坐标
        let rects = rects_pt
            .iter()
            .map(|r| [(r[0] + ox) * scale, (r[1] + oy) * scale, r[2] * scale])
            .collect();
        let tray_rects = tray_rects_pt
            .iter()
            .map(|r| [(r[0] + ox) * scale, (r[1] + oy) * scale, r[2] * scale])
            .collect();
        let desktop_rect = desktop_rect_pt
            .map(|r| [(r[0] + ox) * scale, (r[1] + oy) * scale, r[2] * scale, r[3] * scale]);

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
        let input = self.soft.input(fw, fh);
        let mut out = ctx.run_ui(
            input,
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
        self.soft.update_textures(&mut out.textures_delta);
        let rgba = self.soft.rasterize(out.shapes, out.pixels_per_point, w, h);
        (w, h, rgba)
    }

    // ---------- 同程序多开：窗口选择弹窗 ----------

    /// 弹窗尺寸（逻辑点 → 物理像素）。宽度按最长标题撑开，夹在 MIN/MAX 之间。
    pub fn winlist_size(&mut self, titles: &[String], scale: f32) -> (i32, i32) {
        if titles.is_empty() {
            return (0, 0);
        }
        let scale = scale.max(0.5);
        let font = egui::FontId::proportional(cfg::WINLIST_FONT);
        let widest = titles
            .iter()
            .map(|t| self.soft.text_width(t, font.clone()))
            .fold(0.0f32, f32::max);
        let text_w = widest.clamp(
            cfg::WINLIST_MIN_W - cfg::WINLIST_PAD * 2.0 - 18.0,
            cfg::WINLIST_MAX_W - cfg::WINLIST_PAD * 2.0 - 18.0,
        );
        let fw = text_w + cfg::WINLIST_PAD * 2.0 + 18.0;
        let fh = titles.len() as f32 * cfg::WINLIST_ROW_H + cfg::WINLIST_PAD * 2.0;
        ((fw * scale).ceil() as i32, (fh * scale).ceil() as i32)
    }

    /// 画窗口选择弹窗：一行一个窗口标题，悬停行高亮。返回 (预乘 RGBA, 每行命中矩形[px])
    pub fn paint_winlist(
        &mut self,
        titles: &[String],
        hovered: Option<usize>,
        scale: f32,
        w: i32,
        h: i32,
    ) -> (Vec<u8>, Vec<[f32; 4]>) {
        if titles.is_empty() || w <= 0 || h <= 0 {
            return (Vec::new(), Vec::new());
        }
        let scale = scale.max(0.5);
        self.soft.set_scale(scale);
        let fw = w as f32 / scale;
        let fh = h as f32 / scale;
        let pad = cfg::WINLIST_PAD;
        let row_h = cfg::WINLIST_ROW_H;
        let font = egui::FontId::proportional(cfg::WINLIST_FONT);
        // 文本可用宽度（右侧留 18 给"前台"小点，别顶着边）
        let text_w = fw - pad * 2.0 - 18.0;
        // 逐条截断（度量走 SoftBackend，pass 之前也能用）
        let mut labels: Vec<String> = Vec::with_capacity(titles.len());
        for t in titles {
            let t = t.trim();
            let mut s = if t.is_empty() { "（无标题）".to_string() } else { t.to_string() };
            if self.soft.text_width(&s, font.clone()) > text_w {
                let mut chars: Vec<char> = s.chars().collect();
                while self.soft.text_width(&format!("{}…", chars.iter().collect::<String>()), font.clone())
                    > text_w
                    && chars.len() > 1
                {
                    chars.pop();
                }
                s = format!("{}…", chars.iter().collect::<String>());
            }
            labels.push(s);
        }

        let mut rows_pt: Vec<[f32; 4]> = Vec::with_capacity(labels.len());
        let ctx = self.soft.ctx().clone();
        let input = self.soft.input(fw, fh);
        let mut out = ctx.run_ui(
            input,
            |ui| {
                let p = ui.painter().clone();
                let board = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh));
                let rad = egui::CornerRadius::same(cfg::cur().panel_radius.min(12.0) as u8);
                // 按胶囊的画法：只铺底，不画描边（描边羽化会溢出窗口边界成白边）
                p.rect_filled(board, rad, argb(cfg::cur().panel_bg));
                for (i, label) in labels.iter().enumerate() {
                    let ry = pad + i as f32 * row_h;
                    let row = egui::Rect::from_min_size(
                        egui::pos2(pad * 0.5, ry),
                        egui::vec2(fw - pad, row_h - 2.0),
                    );
                    rows_pt.push([row.min.x, row.min.y, row.width(), row.height()]);
                    if Some(i) == hovered {
                        p.rect_filled(row, egui::CornerRadius::same(8), argb(cfg::cur().hover));
                    }
                    p.text(
                        egui::pos2(row.min.x + 9.0, row.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        font.clone(),
                        argb(cfg::cur().text_body),
                    );
                }
            },
        );
        self.soft.update_textures(&mut out.textures_delta);
        let rgba = self.soft.rasterize(out.shapes, out.pixels_per_point, w, h);
        let rows = rows_pt
            .iter()
            .map(|r| [r[0] * scale, r[1] * scale, r[2] * scale, r[3] * scale])
            .collect();
        (rgba, rows)
    }
}
