//! 展开面板的 egui 软件后端绘制：通用列表面板 + 天气宽卡片。
//! 尺寸计算仍复用 `render::panel_size` / `weather_panel_size`（纯字体测量，供定位预算），
//! 这里按 w/h（物理像素）反推逻辑点空间，用 egui 画文本/底/描边，输出预乘 RGBA + 命中区。
//! 命中区动作号协议不变：0..999 = 行号，PIN_BASE(1000)+ 行号 = 固定按钮，TAB_BASE(2000)+ 序号 = 标签页。

use eframe::egui;

use crate::config as cfg;
use crate::egui_soft::{argb, ellipsize, widget_icon, with_alpha, KIND_USB, SoftBackend};
use crate::render::{PanelRow, UsbView, WeatherView, LEFT_DET_DY, LEFT_DET_STEP, LEFT_REP_GAP, PIN_BASE, TAB_BASE, USB_EJECT_BASE};

pub struct PanelPainted {
    /// 预乘 RGBA（w*h*4）
    pub rgba: Vec<u8>,
    /// 命中区域（物理像素 [x, y, w, h], 动作号）
    pub hits: Vec<([f32; 4], u32)>,
}

// ---- 文本度量工具（走 SoftBackend 的独立度量通道，pass 之前也能用）----

fn tw(soft: &SoftBackend, text: &str, size: f32) -> f32 {
    soft.text_width(text, egui::FontId::proportional(size))
}

fn th(soft: &SoftBackend, text: &str, size: f32) -> f32 {
    soft.text_height(text, egui::FontId::proportional(size))
}

fn txt(p: &egui::Painter, x: f32, y: f32, s: &str, size: f32, color: u32) {
    p.text(
        egui::pos2(x, y),
        egui::Align2::LEFT_TOP,
        s,
        egui::FontId::proportional(size),
        argb(color),
    );
}

/// 命中矩形：逻辑点 → 物理像素
fn to_px(hits: &[([f32; 4], u32)], scale: f32) -> Vec<([f32; 4], u32)> {
    hits.iter()
        .map(|(r, a)| ([r[0] * scale, r[1] * scale, r[2] * scale, r[3] * scale], *a))
        .collect()
}

// ---- 通用列表面板 ----

pub fn paint_list(
    soft: &mut SoftBackend,
    w: i32, h: i32, scale: f32,
    title: &str, subtitle: &str,
    rows: &[PanelRow], empty: &str,
    tabs: &[String], tab_active: usize,
    hovered: Option<u32>,
) -> PanelPainted {
    if w <= 0 || h <= 0 {
        return PanelPainted { rgba: Vec::new(), hits: Vec::new() };
    }
    let scale = scale.max(0.5);
    soft.set_scale(scale);
    let fw = w as f32 / scale;
    let fh = h as f32 / scale;

    let pad = cfg::PANEL_PAD;
    let head_h = cfg::PANEL_HEAD_H - if subtitle.is_empty() { 12.0 } else { 0.0 };
    let s = &*soft; // 不可变借用做字体度量
    let mut hits: Vec<([f32; 4], u32)> = Vec::new();

    let ctx = s.ctx().clone();
    // ppp 每帧走 RawInput（见 egui_soft::input 的注释），否则缩放 ≠ 1.0 时整版错位
    let input = s.input(fw, fh);
    let mut out = ctx.run_ui(
        input,
        |ui| {
            let p = ui.painter().clone();
            // 底板 + 描边
            p.rect_filled(
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh)),
                egui::CornerRadius::same(cfg::cur().panel_radius as u8),
                argb(cfg::bg_alpha(cfg::cur().panel_bg)),
            );
            // 按胶囊的画法：只铺底不描边（描边羽化溢出窗口边界会成白边）

            // 标题
            let title_y = if subtitle.is_empty() {
                (head_h - th(s, title, cfg::PANEL_TITLE_SIZE)) / 2.0
            } else {
                head_h / 2.0 - th(s, title, cfg::PANEL_TITLE_SIZE) + 2.0
            };
            txt(&p, pad, title_y, title, cfg::PANEL_TITLE_SIZE, cfg::cur().text);
            if !subtitle.is_empty() {
                let sh = th(s, subtitle, cfg::PANEL_SUB_SIZE);
                txt(&p, pad, head_h - sh - 4.0, subtitle, cfg::PANEL_SUB_SIZE, cfg::cur().text_dim);
            }

            // 右上标签页
            if !tabs.is_empty() {
                let tab_h = 22.0;
                let ty = (head_h - tab_h) / 2.0;
                let mut tabs_total = 0.0;
                for t in tabs {
                    tabs_total += tw(s, t, cfg::PANEL_SUB_SIZE) + 18.0 + 4.0;
                }
                let mut x = fw - pad - tabs_total;
                for (k, label) in tabs.iter().enumerate() {
                    let label_w = tw(s, label, cfg::PANEL_SUB_SIZE);
                    let twid = label_w + 18.0;
                    let rect = [x, ty, twid, tab_h];
                    let active = k == tab_active;
                    if active {
                        p.rect_filled(egui::Rect::from_min_size(egui::pos2(x, ty), egui::vec2(twid, tab_h)),
                            egui::CornerRadius::same(HOT_R), argb(cfg::cur().hover));
                    } else if hovered == Some(TAB_BASE + k as u32) {
                        p.rect_filled(egui::Rect::from_min_size(egui::pos2(x, ty), egui::vec2(twid, tab_h)),
                            egui::CornerRadius::same(HOT_R), argb(with_alpha(cfg::cur().text, 0.08)));
                    }
                    let lx = x + (twid - label_w) / 2.0;
                    let ly = ty + (tab_h - th(s, label, cfg::PANEL_SUB_SIZE)) / 2.0;
                    let c = if active { cfg::cur().text } else { cfg::cur().text_dim };
                    txt(&p, lx, ly, label, cfg::PANEL_SUB_SIZE, c);
                    hits.push((rect, TAB_BASE + k as u32));
                    x += twid + 4.0;
                }
            }

            // 行列表
            let row_h = cfg::PANEL_ROW_H;
            if rows.is_empty() {
                let eh = th(s, empty, cfg::PANEL_ROW_SUB);
                txt(&p, pad, head_h + (row_h - eh) / 2.0, empty, cfg::PANEL_ROW_SUB, cfg::cur().text_dim);
            }
            for (i, row) in rows.iter().enumerate() {
                let y = head_h + i as f32 * row_h;
                let id = i as u32;
                hits.push(([pad * 0.5, y, fw - pad, row_h], id));
                if hovered == Some(id) {
                    p.rect_filled(
                        egui::Rect::from_min_size(egui::pos2(pad * 0.5, y + 1.0), egui::vec2(fw - pad, row_h - 2.0)),
                        egui::CornerRadius::same(HOT_R), argb(cfg::cur().hover));
                }

                let mut x = pad;
                if !row.icon.is_empty() {
                    let ih = th(s, &row.icon, cfg::WIDGET_ICON_SIZE);
                    txt(&p, x, y + (row_h - ih) / 2.0, &row.icon, cfg::WIDGET_ICON_SIZE, row.icon_argb);
                    x += tw(s, &row.icon, cfg::WIDGET_ICON_SIZE) + cfg::PANEL_ICON_GAP;
                }

                let pin_w = if row.pin.is_some() { cfg::PIN_W } else { 0.0 };
                let vw = if row.value.is_empty() { 0.0 } else { tw(s, &row.value, cfg::PANEL_VALUE_SIZE) };
                let room = (fw - pad - pin_w - vw - cfg::PANEL_VALUE_GAP - x).max(24.0);
                if row.sub.is_empty() {
                    let tt = ellipsize(s, &row.title, room, cfg::PANEL_ROW_TITLE);
                    let tth = th(s, &tt, cfg::PANEL_ROW_TITLE);
                    txt(&p, x, y + (row_h - tth) / 2.0, &tt, cfg::PANEL_ROW_TITLE, cfg::cur().text_body);
                } else {
                    let tt = ellipsize(s, &row.title, room, cfg::PANEL_ROW_TITLE);
                    txt(&p, x, y + 8.0, &tt, cfg::PANEL_ROW_TITLE, cfg::cur().text_body);
                    let ss = ellipsize(s, &row.sub, room, cfg::PANEL_ROW_SUB);
                    txt(&p, x, y + 26.0, &ss, cfg::PANEL_ROW_SUB, cfg::cur().text_dim);
                }
                if vw > 0.0 {
                    let rh = th(s, &row.value, cfg::PANEL_VALUE_SIZE);
                    txt(&p, fw - pad - pin_w - vw, y + (row_h - rh) / 2.0,
                        &row.value, cfg::PANEL_VALUE_SIZE, row.value_argb);
                }
                if let Some(pinned) = row.pin {
                    let (star, color) = if pinned { ("★", cfg::cur().pin) } else { ("☆", cfg::cur().icon_dim) };
                    let sw = tw(s, star, 13.0);
                    let sh = th(s, star, 13.0);
                    let sx = fw - pad - pin_w + (pin_w - sw) / 2.0;
                    txt(&p, sx, y + (row_h - sh) / 2.0, star, 13.0, color);
                    hits.push(([fw - pad - pin_w, y, pin_w, row_h], PIN_BASE + id));
                }
            }
        },
    );
    soft.update_textures(&mut out.textures_delta);
    let rgba = soft.rasterize(out.shapes, out.pixels_per_point, w, h);
    PanelPainted { rgba, hits: to_px(&hits, scale) }
}

// ---- 天气面板 ----

/// 天气卡片左栏各元素相对块顶部的偏移（逻辑单位，会再乘字号倍率）。
/// DET/STEP/REP_GAP 的单一来源在 render.rs（定高公式 left_group_height 共用）。
const LEFT_SUB_DY: f32 = 58.0;

/// 面板内可点元素（列表行 / 标签页 / USB 按钮 / 天气刷新）悬停底的统一圆角。
/// 原先行 8 / 标签页与 USB 按钮 7 / 天气刷新 6 混用：同一窗口内的层级感
/// 应该靠色深区分而不是圆角差异，收敛成一个值（与 Dock 提示条、开始菜单
/// 列表行的 8 对齐；更大圆角只留给尺寸明显更大的开始菜单磁贴格）。
const HOT_R: u8 = 8;

pub fn paint_weather(
    soft: &mut SoftBackend,
    w: i32, h: i32, scale: f32,
    v: &WeatherView, hover_refresh: bool,
) -> PanelPainted {
    if w <= 0 || h <= 0 {
        return PanelPainted { rgba: Vec::new(), hits: Vec::new() };
    }
    let scale = scale.max(0.5);
    soft.set_scale(scale);
    let fw = w as f32 / scale;
    let fh = h as f32 / scale;
    let k = crate::settings::weather_text_scale();

    let pad = 16.0;
    let s = &*soft;
    let mut hits: Vec<([f32; 4], u32)> = Vec::new();

    let ctx = s.ctx().clone();
    // ppp 每帧走 RawInput（见 egui_soft::input 的注释），否则缩放 ≠ 1.0 时整版错位
    let input = s.input(fw, fh);
    let mut out = ctx.run_ui(
        input,
        |ui| {
            let p = ui.painter().clone();
            // 底板（按胶囊的画法：只铺底不描边，描边羽化会溢出窗口成白边）
            p.rect_filled(
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh)),
                egui::CornerRadius::same(cfg::cur().panel_radius as u8), argb(cfg::bg_alpha(cfg::cur().panel_bg)));

            // 标题：当地 + 天气（强调色）
            let t1w = tw(s, "当地", 15.0 * k);
            txt(&p, pad, 9.0 * k, "当地", 15.0 * k, cfg::cur().text);
            txt(&p, pad + t1w, 9.0 * k, "天气", 15.0 * k, cfg::cur().accent);

            // 右上刷新
            let refresh_w = 28.0 * k;
            let refresh_rect = [fw - pad - refresh_w, 7.0 * k, refresh_w, 22.0 * k];
            if hover_refresh {
                p.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(refresh_rect[0] - 6.0, refresh_rect[1]),
                        egui::vec2(refresh_rect[2] + 12.0, refresh_rect[3])),
                    egui::CornerRadius::same(HOT_R), argb(cfg::cur().hover));
            }
            let rw = tw(s, "刷新", cfg::WEATHER_DETAIL_SIZE * k);
            let rh = th(s, "刷新", cfg::WEATHER_DETAIL_SIZE * k);
            txt(&p, refresh_rect[0] + (refresh_w - rw) / 2.0,
                refresh_rect[1] + (refresh_rect[3] - rh) / 2.0,
                "刷新", cfg::WEATHER_DETAIL_SIZE * k, cfg::cur().text_dim);

            // 时间 + 日期（右对齐到刷新左侧）
            let twid = tw(s, &v.time, 14.0 * k);
            let thh = th(s, &v.time, 14.0 * k);
            let tx = refresh_rect[0] - 14.0 * k - twid;
            txt(&p, tx, 9.0 * k, &v.time, 14.0 * k, cfg::cur().text);
            let dw = tw(s, &v.date, cfg::WEATHER_DETAIL_SIZE * k);
            let dh = th(s, &v.date, cfg::WEATHER_DETAIL_SIZE * k);
            txt(&p, tx - 8.0 * k - dw, 9.0 * k + (thh - dh) / 2.0,
                &v.date, cfg::WEATHER_DETAIL_SIZE * k, cfg::cur().text_dim);

            // ---- 左：实况 ----
            let top = 34.0 * k;
            let icon_box = 60.0 * k;
            let iw = tw(s, &v.icon, cfg::WEATHER_ICON_SIZE * k);
            let temp_w = tw(s, &v.temp, cfg::WEATHER_TEMP_SIZE * k);
            let cond_w = tw(s, &v.condition, cfg::WEATHER_COND_SIZE * k);
            let cond_h = th(s, &v.condition, cfg::WEATHER_COND_SIZE * k);
            let city_w = tw(s, &v.city, cfg::WEATHER_CITY_SIZE * k);
            let city_h = th(s, &v.city, cfg::WEATHER_CITY_SIZE * k);
            let det_w = v.details.iter().take(2)
                .map(|l| tw(s, l, cfg::WEATHER_DETAIL_SIZE * k)).fold(0.0f32, f32::max);
            let rep_w = tw(s, &v.report, 10.0 * k);
            let cap = 300.0 * k;
            let row1 = icon_box + 10.0 * k + temp_w;
            let row2 = cond_w + 8.0 * k + city_w;
            let mut left_w = row1.max(row2).max(det_w).max(rep_w).min(cap);
            left_w = left_w.max(row1).max(row2) + 12.0 * k;

            // ① 图标 + 大号温度
            txt(&p, pad + (icon_box - iw) / 2.0, top, &v.icon, cfg::WEATHER_ICON_SIZE * k, v.icon_argb);
            txt(&p, pad + icon_box + 10.0 * k, top + 2.0 * k, &v.temp, cfg::WEATHER_TEMP_SIZE * k, cfg::cur().text);
            // ② 天气 · 城市
            let sub_y = top + LEFT_SUB_DY * k;
            txt(&p, pad, sub_y, &v.condition, cfg::WEATHER_COND_SIZE * k, cfg::cur().text);
            txt(&p, pad + cond_w + 8.0 * k, sub_y + (cond_h - city_h),
                &v.city, cfg::WEATHER_CITY_SIZE * k, cfg::cur().text_dim);
            // ③ 详情 + 来源
            let mut y = top + LEFT_DET_DY * k;
            for line in v.details.iter().take(2) {
                let t = ellipsize(s, line, left_w, cfg::WEATHER_DETAIL_SIZE * k);
                txt(&p, pad, y, &t, cfg::WEATHER_DETAIL_SIZE * k, cfg::cur().text_dim);
                y += LEFT_DET_STEP * k;
            }
            txt(&p, pad, y + LEFT_REP_GAP * k, &v.report, 10.0 * k, cfg::cur().icon_dim);

            // 竖分隔线
            let divx = pad + left_w + 10.0 * k;
            p.rect_filled(
                egui::Rect::from_min_size(egui::pos2(divx, top), egui::vec2(1.0, 140.0 * k)),
                0.0_f32, argb(cfg::cur().divider));

            // ---- 右：四天 ----
            let days_y = 44.0 * k;
            let cols = v.days.len().min(4) as f32;
            if cols > 0.0 {
                let start = divx + 12.0 * k;
                let cw = (fw - start - pad) / cols;
                for (i, d) in v.days.iter().take(4).enumerate() {
                    let center = start + i as f32 * cw + cw / 2.0;
                    let argb_day = if i == 0 { cfg::cur().text } else { cfg::cur().text_dim };

                    let lw = tw(s, &d.label, cfg::WEATHER_DAY_LABEL * k);
                    txt(&p, center - lw / 2.0, days_y + 1.0 * k, &d.label, cfg::WEATHER_DAY_LABEL * k, argb_day);
                    let icw = tw(s, &d.icon, cfg::WEATHER_DAY_ICON * k);
                    txt(&p, center - icw / 2.0, days_y + 21.0 * k, &d.icon, cfg::WEATHER_DAY_ICON * k, d.icon_argb);
                    let hw = tw(s, &d.high, cfg::WEATHER_DAY_HIGH * k);
                    txt(&p, center - hw / 2.0, days_y + 46.0 * k, &d.high, cfg::WEATHER_DAY_HIGH * k, cfg::cur().text);
                    let lw2 = tw(s, &d.low, cfg::WEATHER_DAY_LOW * k);
                    txt(&p, center - lw2 / 2.0, days_y + 66.0 * k, &d.low, cfg::WEATHER_DAY_LOW * k, cfg::cur().text_dim);
                    if !d.text.is_empty() {
                        let t = ellipsize(s, &d.text, cw - 2.0, cfg::WEATHER_DAY_TEXT * k);
                        let tww = tw(s, &t, cfg::WEATHER_DAY_TEXT * k);
                        txt(&p, center - tww / 2.0, days_y + 86.0 * k, &t, cfg::WEATHER_DAY_TEXT * k, cfg::cur().text_body);
                    }
                    if !d.note.is_empty() {
                        let t = ellipsize(s, &d.note, cw - 2.0, cfg::WEATHER_DAY_WIND * k);
                        let nw = tw(s, &t, cfg::WEATHER_DAY_WIND * k);
                        txt(&p, center - nw / 2.0, days_y + 103.0 * k, &t, cfg::WEATHER_DAY_WIND * k, d.note_argb);
                    }
                }
            }
            hits.push((refresh_rect, 0));
        },
    );
    soft.update_textures(&mut out.textures_delta);
    let rgba = soft.rasterize(out.shapes, out.pixels_per_point, w, h);
    PanelPainted { rgba, hits: to_px(&hits, scale) }
}

// ---- USB 面板 ----

/// 布局协议：0..999 = 行（点行 = 资源管理器打开该盘），USB_EJECT_BASE + 行号 = 「弹出」按钮。
/// 命中区顺序：行先压入、弹出按钮后压入（`action_at` 后压入的优先，按钮压在整行上面）。
pub fn paint_usb(
    soft: &mut SoftBackend,
    w: i32, h: i32, scale: f32,
    v: &UsbView,
    hovered: Option<u32>,
) -> PanelPainted {
    if w <= 0 || h <= 0 {
        return PanelPainted { rgba: Vec::new(), hits: Vec::new() };
    }
    let scale = scale.max(0.5);
    soft.set_scale(scale);
    let fw = w as f32 / scale;
    let fh = h as f32 / scale;

    let pad = cfg::PANEL_PAD;
    let head_h = cfg::PANEL_HEAD_H;
    let row_h = cfg::PANEL_ROW_H;
    let btn_h = 22.0;
    let s = &*soft;
    let mut hits: Vec<([f32; 4], u32)> = Vec::new();

    // 按钮尺寸（两处绘制一致，宽度从文本量出来）
    let open_w = tw(s, "打开", USB_BTN_SIZE) + 16.0;
    let ej_w = tw(s, "弹出", USB_BTN_SIZE) + 16.0;

    let ctx = s.ctx().clone();
    let input = s.input(fw, fh);
    let mut out = ctx.run_ui(
        input,
        |ui| {
            let p = ui.painter().clone();
            p.rect_filled(
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh)),
                egui::CornerRadius::same(cfg::cur().panel_radius as u8),
                argb(cfg::bg_alpha(cfg::cur().panel_bg)),
            );

            // 标题 + 副标题（与列表面板同一套节奏）
            let title_y = head_h / 2.0 - th(s, &v.title, cfg::PANEL_TITLE_SIZE) + 2.0;
            txt(&p, pad, title_y, &v.title, cfg::PANEL_TITLE_SIZE, cfg::cur().text);
            if !v.subtitle.is_empty() {
                let sh = th(s, &v.subtitle, cfg::PANEL_SUB_SIZE);
                txt(&p, pad, head_h - sh - 4.0, &v.subtitle, cfg::PANEL_SUB_SIZE, cfg::cur().text_dim);
            }

            for (i, row) in v.rows.iter().enumerate() {
                let y = head_h + i as f32 * row_h;
                let id = i as u32;
                hits.push(([pad * 0.5, y, fw - pad, row_h], id));
                if hovered == Some(id) {
                    p.rect_filled(
                        egui::Rect::from_min_size(egui::pos2(pad * 0.5, y + 1.0),
                            egui::vec2(fw - pad, row_h - 2.0)),
                        egui::CornerRadius::same(HOT_R), argb(cfg::cur().hover));
                }

                // 矢量 U 盘图标
                let icx = pad + cfg::WIDGET_ICON_SIZE * 0.5 + 1.0;
                widget_icon(&p, egui::pos2(icx, y + row_h / 2.0),
                    cfg::WIDGET_ICON_SIZE, KIND_USB, argb(cfg::cur().icon_dim));
                let x = pad + cfg::WIDGET_ICON_SIZE + cfg::PANEL_ICON_GAP;

                // 右侧按钮区（弹出贴右缘，打开在其左）
                let ej_x = fw - pad - ej_w;
                let open_x = ej_x - 6.0 - open_w;
                let by = y + (row_h - btn_h) / 2.0;
                let room = (open_x - x - 8.0).max(24.0);

                let tt = ellipsize(s, &row.title, room, cfg::PANEL_ROW_TITLE);
                txt(&p, x, y + 8.0, &tt, cfg::PANEL_ROW_TITLE, cfg::cur().text_body);
                let ss = ellipsize(s, &row.sub, room, cfg::PANEL_ROW_SUB);
                txt(&p, x, y + 26.0, &ss, cfg::PANEL_ROW_SUB, cfg::cur().text_dim);

                // 「打开」按钮：描边 + 常规色
                let open_rect = egui::Rect::from_min_size(egui::pos2(open_x, by), egui::vec2(open_w, btn_h));
                let open_hot = hovered == Some(id) || hovered == Some(USB_EJECT_BASE + id);
                if open_hot {
                    p.rect_filled(open_rect, egui::CornerRadius::same(HOT_R), argb(cfg::cur().hover));
                }
                p.rect_stroke(open_rect, egui::CornerRadius::same(HOT_R),
                    egui::Stroke::new(1.0, argb(cfg::cur().icon_dim)), egui::StrokeKind::Inside);
                let ow = tw(s, "打开", USB_BTN_SIZE);
                let oh = th(s, "打开", USB_BTN_SIZE);
                txt(&p, open_x + (open_w - ow) / 2.0, by + (btn_h - oh) / 2.0,
                    "打开", USB_BTN_SIZE, cfg::cur().text_dim);

                // 「弹出」按钮：强调色（安全移除是主操作）
                let ej_rect = egui::Rect::from_min_size(egui::pos2(ej_x, by), egui::vec2(ej_w, btn_h));
                let ej_hot = hovered == Some(USB_EJECT_BASE + id);
                if ej_hot {
                    p.rect_filled(ej_rect, egui::CornerRadius::same(HOT_R), argb(cfg::cur().hover));
                }
                p.rect_stroke(ej_rect, egui::CornerRadius::same(HOT_R),
                    egui::Stroke::new(1.0, argb(cfg::cur().accent)), egui::StrokeKind::Inside);
                let ew = tw(s, "弹出", USB_BTN_SIZE);
                let eh = th(s, "弹出", USB_BTN_SIZE);
                txt(&p, ej_x + (ej_w - ew) / 2.0, by + (btn_h - eh) / 2.0,
                    "弹出", USB_BTN_SIZE, cfg::cur().accent);

                // 弹出按钮的命中区压在整行上面（后压入优先）
                hits.push(([ej_x, by, ej_w, btn_h], USB_EJECT_BASE + id));
            }
        },
    );
    soft.update_textures(&mut out.textures_delta);
    let rgba = soft.rasterize(out.shapes, out.pixels_per_point, w, h);
    PanelPainted { rgba, hits: to_px(&hits, scale) }
}

/// 按钮字号（usb_panel_size 与 paint_usb 共用的数值）
const USB_BTN_SIZE: f32 = 12.5;
