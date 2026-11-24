//! 音乐条的 egui 软件后端绘制：圆角底 + 左侧旋转圆形封面（中心播放/暂停按钮）+ 右侧歌词。
//! 封面旋转仍复用 `render::rotate_cover`（纯像素运算），结果上传成 egui 纹理再画。
//! egui 在**逻辑点**空间布局，`egui_soft` 光栅化成**预乘 RGBA**。

use eframe::egui;

use crate::config as cfg;
use crate::egui_soft::{argb, glass_board, with_alpha, SoftBackend};

/// 封面在条内的逻辑位置与边长：(x, y, 边长)（逻辑点，相对窗口左上角）。
/// 与 render::cover_box 的数值口径一致（乘 scale 即物理像素）。
pub fn cover_box(fh: f32) -> (f32, f32, f32) {
    let d = cfg::MUSIC_COVER.min(fh - 6.0).max(8.0);
    (cfg::MUSIC_COVER_PAD, (fh - d) / 2.0, d)
}

/// 除歌词外的逻辑固定宽度。
pub fn fixed_w() -> f32 {
    cfg::MUSIC_COVER_PAD + cfg::MUSIC_COVER + cfg::MUSIC_COVER_GAP + cfg::LYRIC_PAD_X
}

/// 歌词文本的逻辑宽度（走 SoftBackend 的独立度量通道：pass 之前也能量）。
pub fn text_w(soft: &SoftBackend, text: &str, font: f32) -> f32 {
    soft.text_width(text, egui::FontId::proportional(font))
}

/// 按最大逻辑宽度截断并加省略号（egui 字体度量）。
pub fn ellipsize(soft: &SoftBackend, text: &str, max_w: f32, font: f32) -> String {
    if text_w(soft, text, font) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut trial = out.clone();
        trial.push(ch);
        trial.push('…');
        if text_w(soft, &trial, font) > max_w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// 画整条音乐条。w/h 是物理像素尺寸，scale 是 pixels_per_point。
/// cover 是 decode_cover 解码后的正方形 straight RGBA，rotate_cover 在这里调用。
/// 形状固定为四角圆角胶囊（不跟随顶部胶囊的吸顶/吸底"耳朵"）。
pub fn paint(
    soft: &mut SoftBackend,
    w: i32, h: i32, scale: f32, text: &str, font: f32,
    cover: Option<&[u8]>, angle: f32, playing: bool, hovered: bool,
) -> Vec<u8> {
    if w <= 0 || h <= 0 {
        return Vec::new();
    }
    let scale = scale.max(0.5);
    soft.set_scale(scale);
    let fw = w as f32 / scale;
    let fh = h as f32 / scale;

    let ctx = soft.ctx().clone();
    let soft_ref = &mut *soft;
    // ppp 每帧走 RawInput（见 egui_soft::input 的注释），否则缩放 ≠ 1.0 时整版错位
    let input = soft_ref.input(fw, fh);
    let mut out = ctx.run_ui(
        input,
        |ui| {
            let p = ui.painter().clone();
            // 圆角胶囊底，与顶部胶囊同一画法
            glass_board(
                &p,
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh)),
                egui::CornerRadius::same(crate::settings::corner_radius_for(fh).round() as u8),
            );

            let (cx0, cy0, d) = cover_box(fh);
            let center = egui::pos2(cx0 + d / 2.0, cy0 + d / 2.0);
            let mut drew_cover = false;
            if let Some(src) = cover.filter(|s| !s.is_empty()) {
                // 纹理按**物理像素**生成（否则 1.25× DPI 下封面会被拉伸发糊）
                let px = (d * scale).round().max(8.0) as i32;
                let rotated = crate::render::rotate_cover(src, px, angle);
                if rotated.len() >= (px as usize) * (px as usize) * 4 {
                    let tex = soft_ref.soft_image(&rotated, px as usize, px as usize);
                    p.image(
                        tex.id(),
                        egui::Rect::from_min_size(egui::pos2(cx0, cy0), egui::vec2(d, d)),
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                    drew_cover = true;
                }
            }
            if !drew_cover {
                // 占位圆 + 音符（圆底跟随主题字色低透明派生，深浅主题都可辨）
                p.circle_filled(center, d / 2.0, argb(with_alpha(cfg::cur().text, 0.15)));
                let icon = cfg::MUSIC_PLACEHOLDER;
                let fid = egui::FontId::proportional(cfg::LYRIC_ICON_SIZE * 1.7);
                p.text(center, egui::Align2::CENTER_CENTER, icon, fid, argb(cfg::cur().accent));
            }
            // 封面细描边（跟随主题字色低透明派生）
            p.circle_stroke(
                center,
                (d - 1.0) / 2.0,
                egui::Stroke::new(1.0, argb(with_alpha(cfg::cur().text, 0.25))),
            );

            // 中心按钮：半透明圆盘 + 播放/暂停。
            // 盘色由主题面板底色派生（面板底色与文字的反差已有对比度单测兜底），
            // 浅色主题不再出现"深色盘 + 深色字"的不可读组合；hover 时盘更实。
            let bd = d * cfg::MUSIC_BTN_RATIO;
            let btn_alpha = if hovered { 0.90 } else { 0.65 };
            p.circle_filled(center, bd / 2.0, argb(with_alpha(cfg::cur().panel_bg, btn_alpha)));
            if playing {
                // 暂停：两根竖条
                let bw = (bd * 0.15).max(2.0);
                let bh = bd * 0.44;
                let gapw = (bd * 0.14).max(1.0);
                let y = center.y - bh / 2.0;
                let r = egui::CornerRadius::same((bw / 2.0).max(1.0) as u8);
                p.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(center.x - gapw / 2.0 - bw, y), egui::vec2(bw, bh)),
                    r,
                    argb(cfg::cur().text),
                );
                p.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(center.x + gapw / 2.0, y), egui::vec2(bw, bh)),
                    r,
                    argb(cfg::cur().text),
                );
            } else {
                // 播放：三角形
                let half_h = bd * 0.22;
                let base_x = center.x - bd * 0.16;
                let tip_x = center.x + bd * 0.22;
                let pts = vec![
                    egui::pos2(base_x, center.y - half_h),
                    egui::pos2(base_x, center.y + half_h),
                    egui::pos2(tip_x, center.y),
                ];
                p.add(egui::Shape::convex_polygon(pts, argb(cfg::cur().text), egui::Stroke::NONE));
            }

            // 歌词（控制在前、歌词在后）
            let tx = cx0 + d + cfg::MUSIC_COVER_GAP;
            p.text(
                egui::pos2(tx, fh / 2.0),
                egui::Align2::LEFT_CENTER,
                text,
                egui::FontId::proportional(font),
                argb(cfg::cur().text_body),
            );
        },
    );

    soft.update_textures(&mut out.textures_delta);
    soft.rasterize(out.shapes, out.pixels_per_point, w, h)
}
