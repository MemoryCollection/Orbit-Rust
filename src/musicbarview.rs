//! 音乐条的 egui 软件后端绘制：圆角底 + 左侧旋转圆形封面（中心播放/暂停按钮）+ 右侧歌词。
//! 封面旋转仍复用 `render::rotate_cover`（纯像素运算），结果上传成 egui 纹理再画。
//! egui 在**逻辑点**空间布局，`egui_soft` 光栅化成**预乘 RGBA**。

use eframe::egui;

use crate::config as cfg;
use crate::egui_soft::{argb, SoftBackend};

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

/// 歌词文本的逻辑宽度（egui 字体度量）。
pub fn text_w(soft: &SoftBackend, text: &str, font: f32) -> f32 {
    let fid = egui::FontId::proportional(font);
    soft.ctx().fonts_mut(|f| {
        f.layout_no_wrap(text.to_string(), fid, egui::Color32::WHITE).size().x
    })
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
    let out = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh))),
            ..Default::default()
        },
        |ui| {
            let p = ui.painter().clone();
            // 圆角底
            p.rect_filled(
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh)),
                egui::CornerRadius::same((fh / 2.0).min(255.0) as u8),
                argb(cfg::BG_ARGB),
            );

            let (cx0, cy0, d) = cover_box(fh);
            let center = egui::pos2(cx0 + d / 2.0, cy0 + d / 2.0);
            let mut drew_cover = false;
            if let Some(src) = cover.filter(|s| !s.is_empty()) {
                let px = d.round().max(8.0) as i32;
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
                // 占位圆 + 音符
                p.circle_filled(center, d / 2.0, argb(0x26_FF_FF_FF));
                let icon = cfg::MUSIC_PLACEHOLDER;
                let fid = egui::FontId::proportional(cfg::LYRIC_ICON_SIZE * 1.7);
                p.text(center, egui::Align2::CENTER_CENTER, icon, fid, argb(cfg::ACCENT_ARGB));
            }
            // 封面细白描边
            p.circle_stroke(
                center,
                (d - 1.0) / 2.0,
                egui::Stroke::new(1.0, argb(0x40_FF_FF_FF)),
            );

            // 中心按钮：半透明圆盘 + 播放/暂停
            let bd = d * cfg::MUSIC_BTN_RATIO;
            let btn_argb = if hovered { 0xE6_14_14_14 } else { 0xA6_10_10_10 };
            p.circle_filled(center, bd / 2.0, argb(btn_argb));
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
                    argb(cfg::TIME_ARGB),
                );
                p.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(center.x + gapw / 2.0, y), egui::vec2(bw, bh)),
                    r,
                    argb(cfg::TIME_ARGB),
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
                p.add(egui::Shape::convex_polygon(pts, argb(cfg::TIME_ARGB), egui::Stroke::NONE));
            }

            // 歌词（控制在前、歌词在后）
            let tx = cx0 + d + cfg::MUSIC_COVER_GAP;
            p.text(
                egui::pos2(tx, fh / 2.0),
                egui::Align2::LEFT_CENTER,
                text,
                egui::FontId::proportional(font),
                argb(cfg::TEXT_ARGB),
            );
        },
    );

    soft.update_textures(&out.textures_delta);
    soft.rasterize(out.shapes, out.pixels_per_point, w, h)
}
