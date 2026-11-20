//! 离线渲染验证：直接引入产品模块，把顶部胶囊与底部 Dock 画成图片存盘，
//! 用来核对分离布局、图标一致性、间距、日期字体、返回桌面细长条与设置卡片。
//! 不开窗口、不碰任务栏。
//!
//! 用法：pillcheck [缩放] [split|merge|pill|shapes|dock|dock_notray|settings]
//!   split（默认）= 一个模块一条：天气在左，邮件与剪贴板各占右侧一条
//!   merge       = 天气在左，邮件 + 剪贴板合并成右侧一条
//!   pill        = 全部留在胶囊里
//!   shapes      = 胶囊三种形状（悬浮 / 吸顶刘海 / 吸底）各画一条
//!   dock        = 底部 Dock（含托盘图标 + 右端返回桌面细长条）
//!   dock_notray = 同上但隐藏托盘图标
//!   settings    = 设置页面卡片（双栏）
//!   输出 tools/_shots/<模式>_<缩放>.bmp

#![allow(dead_code)]

mod settings {
    /// 产品里从 settings.txt 读，这里给缺省值
    pub fn weather_text_scale() -> f32 {
        1.25
    }
}

#[path = "../../../src/config.rs"]
mod config;
#[path = "../../../src/render.rs"]
mod render;

use render::{Chip, FontKind, PillShape, SettingsCtl, SettingsRow, WidgetItem};

/// 把一块 straight-alpha RGBA 贴到画布上（软合成）
fn blit(dst: &mut [u8], dw: i32, src: &[u8], sw: i32, sh: i32, ox: i32, oy: i32) {
    for y in 0..sh {
        for x in 0..sw {
            let si = ((y * sw + x) * 4) as usize;
            let di = (((y + oy) * dw + x + ox) * 4) as usize;
            if di + 3 >= dst.len() || si + 3 >= src.len() {
                continue;
            }
            let a = src[si + 3] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            for k in 0..3 {
                dst[di + k] = (src[si + k] as f32 * a + dst[di + k] as f32 * (1.0 - a)).round() as u8;
            }
        }
    }
}

/// 纯色方块当图标（只为了看布局，不涉及真实图标抓取）
fn solid(px: i32, r: u8, g: u8, b: u8) -> Vec<u8> {
    let n = (px.max(1) * px.max(1) * 4) as usize;
    let mut v = vec![0u8; n];
    for i in 0..(px.max(1) * px.max(1)) as usize {
        v[i * 4] = r;
        v[i * 4 + 1] = g;
        v[i * 4 + 2] = b;
        v[i * 4 + 3] = 0xFF;
    }
    v
}

fn write_bmp(path: &std::path::Path, w: i32, h: i32, rgba: &[u8]) {
    let mut buf: Vec<u8> = Vec::new();
    let row = (w * 4) as usize;
    let size = 54 + row * h as usize;
    buf.extend_from_slice(b"BM");
    buf.extend_from_slice(&(size as u32).to_le_bytes());
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf.extend_from_slice(&54u32.to_le_bytes());
    buf.extend_from_slice(&40u32.to_le_bytes());
    buf.extend_from_slice(&w.to_le_bytes());
    buf.extend_from_slice(&h.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&32u16.to_le_bytes());
    for _ in 0..6 {
        buf.extend_from_slice(&0u32.to_le_bytes());
    }
    for y in (0..h).rev() {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            buf.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], 0xFF]);
        }
    }
    std::fs::write(path, &buf).unwrap();
}

fn shot(name: &str, scale: f32, w: i32, h: i32, rgba: &[u8]) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../_shots");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join(format!("{name}_{scale}.bmp"));
    write_bmp(&path, w, h, rgba);
    println!("wrote {} ({w}x{h})", path.display());
}

fn main() {
    let scale: f32 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1.25);
    let mode = std::env::args().nth(2).unwrap_or_else(|| "split".into());

    match mode.as_str() {
        "dock" | "dock_notray" => dock(scale, mode == "dock"),
        "settings" => settings_page(scale),
        "shapes" => shapes(scale),
        _ => pill(scale, &mode),
    }
}

fn settings_page(scale: f32) {
    let token = render::startup();
    let rows = vec![
        // ================= 左栏 =================
        SettingsRow {
            label: "时钟格式".into(),
            section: Some("顶部胶囊"),
            col: 0,
            ctl: SettingsCtl::Edit {
                value: "%Y-%m-%d %H:%M:%S %a".into(),
                key: 70,
                wide: 0.0,
            },
        },
        SettingsRow {
            label: "实时预览".into(),
            section: None,
            col: 0,
            ctl: SettingsCtl::Info {
                text: "2026-11-20 20:15:03 周五".into(),
                argb: config::ACCENT_ARGB,
            },
        },
        SettingsRow {
            label: "透明度".into(),
            section: None,
            col: 0,
            ctl: SettingsCtl::Step {
                value: "0.85".into(),
                dec: 65,
                inc: 66,
                can_dec: true,
                can_inc: true,
                val_w: 0.0,
            },
        },
        SettingsRow {
            label: "形状".into(),
            section: None,
            col: 0,
            ctl: SettingsCtl::Seg { options: &["悬浮", "吸顶刘海", "吸底"], active: 1, base: 10 },
        },
        SettingsRow {
            label: "位置".into(),
            section: Some("Dock"),
            col: 0,
            ctl: SettingsCtl::Seg { options: &["下", "上", "左", "右"], active: 0, base: 20 },
        },
        SettingsRow {
            label: "位置".into(),
            section: Some("音乐条"),
            col: 0,
            ctl: SettingsCtl::Seg { options: &["左", "右", "下"], active: 1, base: 30 },
        },
        SettingsRow {
            label: "歌词字号".into(),
            section: None,
            col: 0,
            ctl: SettingsCtl::Step {
                value: "14".into(),
                dec: 60,
                inc: 61,
                can_dec: true,
                can_inc: true,
                val_w: 0.0,
            },
        },
        SettingsRow {
            label: "字号倍率".into(),
            section: Some("天气卡片"),
            col: 0,
            ctl: SettingsCtl::Step {
                value: "1.25×".into(),
                dec: 50,
                inc: 51,
                can_dec: true,
                can_inc: true,
                val_w: 0.0,
            },
        },
        SettingsRow {
            label: "数据源".into(),
            section: Some("天气"),
            col: 0,
            ctl: SettingsCtl::Seg { options: &["高德", "Open-Meteo"], active: 0, base: 40 },
        },
        SettingsRow {
            label: "高德 Key".into(),
            section: None,
            col: 0,
            ctl: SettingsCtl::Edit { value: "a1b2c3d4e5f6".into(), key: 71, wide: 0.0 },
        },
        SettingsRow {
            label: "服务器".into(),
            section: Some("邮箱账户"),
            col: 0,
            ctl: SettingsCtl::Edit { value: "imap.qq.com".into(), key: 72, wide: 0.0 },
        },
        SettingsRow {
            label: "端口".into(),
            section: None,
            col: 0,
            ctl: SettingsCtl::Edit { value: "993".into(), key: 73, wide: 0.0 },
        },
        // ================= 右栏 =================
        SettingsRow {
            label: "拖动排序".into(),
            section: Some("顶部胶囊模块"),
            col: 1,
            ctl: SettingsCtl::Chips {
                items: vec![
                    Chip { label: "时间日期".into(), state: 0, key: 100, fixed: true },
                    Chip { label: "天气".into(), state: 1, key: 101, fixed: false },
                    Chip { label: "邮箱".into(), state: 2, key: 102, fixed: false },
                    Chip { label: "剪贴板".into(), state: 3, key: 103, fixed: false },
                ],
            },
        },
        SettingsRow {
            label: "提示".into(),
            section: None,
            col: 1,
            ctl: SettingsCtl::Info {
                text: "点击芯片切换：胶囊 → 左条 → 右条 → 隐藏".into(),
                argb: 0x8C_FF_FF_FF,
            },
        },
        SettingsRow {
            label: "开关".into(),
            section: Some("托盘图标"),
            col: 1,
            ctl: SettingsCtl::Empty,
        },
        SettingsRow {
            label: String::new(),
            section: None,
            col: 1,
            ctl: SettingsCtl::Grid {
                cells: vec![
                    render::GridCell { label: "返回桌面".into(), on: true, key: 198 },
                    render::GridCell { label: "微信".into(), on: true, key: 200 },
                ],
            },
        },
        SettingsRow {
            label: String::new(),
            section: None,
            col: 1,
            ctl: SettingsCtl::Grid {
                cells: vec![
                    render::GridCell { label: "火绒安全软件".into(), on: true, key: 202 },
                    render::GridCell { label: "workbuddy".into(), on: true, key: 203 },
                ],
            },
        },
        SettingsRow {
            label: "全部恢复显示".into(),
            section: None,
            col: 1,
            ctl: SettingsCtl::Button { text: "恢复", key: 190 },
        },
    ];
    let (w, h) = render::settings_size(&rows, scale);
    // 光标画在第 4 个字符处（时钟格式编辑框），验证 Edit 光标渲染
    let out = render::draw_settings(w, h, scale, &rows, Some(70), 99, Some((70, 4)));

    // 灰底，方便看清卡片圆角与描边
    let mut canvas = vec![0u8; (w * h * 4) as usize];
    for i in 0..(w * h) as usize {
        canvas[i * 4] = 0x33;
        canvas[i * 4 + 1] = 0x4c;
        canvas[i * 4 + 2] = 0x5c;
        canvas[i * 4 + 3] = 0xFF;
    }
    blit(&mut canvas, w, &out.rgba, w, h, 0, 0);
    shot("settings", scale, w, h, &canvas);
    render::shutdown(token);
}

fn dock(scale: f32, show_tray: bool) {
    let token = render::startup();
    let px = (config::DOCK_ICON * scale).round() as i32;
    let apps: Vec<Vec<u8>> = vec![
        solid(px, 0x4C, 0x9A, 0xFF),
        solid(px, 0x8E, 0x6B, 0xFF),
        solid(px, 0x3F, 0xC1, 0x8A),
        solid(px, 0xE8, 0x8B, 0x3A),
    ];
    let trays: Vec<Vec<u8>> = if show_tray {
        let tpx = (config::DOCK_TRAY_ICON * scale).round() as i32;
        vec![
            solid(tpx, 0xD0, 0xD0, 0xD0),
            solid(tpx, 0xB0, 0xC4, 0xE8),
            solid(tpx, 0xE8, 0xC4, 0xB0),
        ]
    } else {
        vec![]
    };
    let icons: Vec<&[u8]> = apps.iter().map(|v| v.as_slice()).collect();
    let ticons: Vec<&[u8]> = trays.iter().map(|v| v.as_slice()).collect();

    let (w, h) = render::dock_size(icons.len(), ticons.len(), scale, show_tray, true, false);
    let out = render::draw_dock(w, h, scale, &icons, Some(1), &ticons, None, show_tray, true, true, false);

    let pad = (16.0 * scale) as i32;
    let (cw, ch) = (w + pad * 2, h + pad * 2);
    let mut canvas = vec![0u8; (cw * ch * 4) as usize];
    for i in 0..(cw * ch) as usize {
        canvas[i * 4] = 0x33;
        canvas[i * 4 + 1] = 0x4c;
        canvas[i * 4 + 2] = 0x5c;
        canvas[i * 4 + 3] = 0xFF;
    }
    blit(&mut canvas, cw, &out.rgba, w, h, pad, pad);
    shot(if show_tray { "dock" } else { "dock_notray" }, scale, cw, ch, &canvas);
    println!("  apps={:?} tray={:?} desktop={:?}", out.rects, out.tray_rects, out.desktop_rect);
    render::shutdown(token);
}

fn pill(scale: f32, mode: &str) {
    let token = render::startup();

    // 日期与时间已合并成一段（clock.rs），这里直接给合并后的字符串
    let clock: Vec<u16> = "11:57 11月20日 周五".encode_utf16().collect();
    let (tw, _) = render::measure_text(&clock, config::TIME_SIZE, FontKind::Latin, scale);

    let weather = WidgetItem {
        icon: "☁".into(),
        icon_argb: config::CLOUD_ARGB,
        text: "7°".into(),
    };
    let mail = WidgetItem {
        icon: "✉".into(),
        icon_argb: config::TEXT_DIM_ARGB,
        text: String::new(),
    };
    let clip = WidgetItem {
        icon: "▤".into(),
        icon_argb: config::TEXT_DIM_ARGB,
        text: String::new(),
    };

    let split = mode != "pill";
    // split：一个模块一条（左 1 条 + 右 2 条）；merge：右侧两条合并成一条
    let pill_items: Vec<WidgetItem> =
        if split { vec![] } else { vec![weather.clone(), mail.clone(), clip.clone()] };
    let left: Vec<WidgetItem> = if split { vec![weather.clone()] } else { vec![] };
    let right1: Vec<WidgetItem> = match mode {
        "merge" => vec![mail.clone(), clip.clone()],
        "pill" => vec![],
        _ => vec![mail.clone()],
    };
    let right2: Vec<WidgetItem> = if mode == "split" { vec![clip.clone()] } else { vec![] };

    let h = (config::PILL_H * scale).ceil() as i32;
    // 胶囊右侧的托盘闪烁图标：一块亮帧 + 一块灭帧（灭帧留空位）
    let fpx = (config::DOCK_ICON * scale).round() as i32;
    let flash_src = solid(fpx, 0x3C, 0x8D, 0xFF);
    let flash = vec![
        render::FlashView { rgba: &flash_src, px: fpx, dim: false },
        render::FlashView { rgba: &flash_src, px: fpx, dim: true },
    ];
    let pill_w = render::content_width(tw, &pill_items, flash.len(), scale).ceil() as i32;
    let lw = render::widget_bar_w(&left, scale).ceil() as i32;
    let r1w = render::widget_bar_w(&right1, scale).ceil() as i32;
    let r2w = render::widget_bar_w(&right2, scale).ceil() as i32;
    let gap = (config::WIDGET_BAR_GAP as f32 * scale).round() as i32;

    let pad = (24.0 * scale) as i32;
    let mut cw = pad * 2 + pill_w;
    if lw > 0 {
        cw += lw + gap;
    }
    if r1w > 0 {
        cw += r1w + gap;
    }
    if r2w > 0 {
        cw += r2w + gap;
    }
    let ch = h + pad * 2;
    let mut canvas = vec![0u8; (cw * ch * 4) as usize];
    for i in 0..(cw * ch) as usize {
        canvas[i * 4] = 0x33;
        canvas[i * 4 + 1] = 0x4c;
        canvas[i * 4 + 2] = 0x5c;
        canvas[i * 4 + 3] = 0xFF;
    }

    let mut x = pad;
    if lw > 0 {
        let bar = render::draw_widget_bar(lw, h, scale, &left, None);
        blit(&mut canvas, cw, &bar.rgba, lw, h, x, pad);
        x += lw + gap;
    }
    let pill = render::draw_pill(pill_w, h, scale, &clock, &pill_items, None, &flash, Some(0), PillShape::Float);
    blit(&mut canvas, cw, &pill.rgba, pill_w, h, x, pad);
    x += pill_w;
    if r1w > 0 {
        let bar = render::draw_widget_bar(r1w, h, scale, &right1, None);
        blit(&mut canvas, cw, &bar.rgba, r1w, h, x + gap, pad);
        x += r1w + gap;
    }
    if r2w > 0 {
        let bar = render::draw_widget_bar(r2w, h, scale, &right2, None);
        blit(&mut canvas, cw, &bar.rgba, r2w, h, x + gap, pad);
    }

    shot(&format!("layout_{mode}"), scale, cw, ch, &canvas);
    println!("  胶囊 {pill_w}x{h}, 左条 {lw}, 右1 {r1w}, 右2 {r2w}");
    render::shutdown(token);
}

/// 三种胶囊形状（悬浮 / 吸顶刘海 / 吸底）纵向排列对比
fn shapes(scale: f32) {
    let token = render::startup();

    let clock: Vec<u16> = "20:15:03 2026-11-20 周五".encode_utf16().collect();
    let (tw, _) = render::measure_text(&clock, config::TIME_SIZE, FontKind::Latin, scale);
    let weather = render::WidgetItem {
        icon: "☀".into(),
        icon_argb: config::ACCENT_ARGB,
        text: "8°".into(),
    };

    let h = (config::PILL_H * scale).ceil() as i32;
    let w = render::content_width(tw, &[weather.clone()], 0, scale).ceil() as i32;
    let all = [
        ("悬浮", PillShape::Float),
        ("吸顶刘海", PillShape::Island),
        ("吸底", PillShape::Bottom),
    ];
    let gap = (20.0 * scale) as i32;
    let pad = (24.0 * scale) as i32;
    let cw = w + pad * 2;
    let ch = all.len() as i32 * h + (all.len() as i32 - 1) * gap + pad * 2;
    let mut canvas = vec![0u8; (cw * ch * 4) as usize];
    for i in 0..(cw * ch) as usize {
        canvas[i * 4] = 0x33;
        canvas[i * 4 + 1] = 0x4c;
        canvas[i * 4 + 2] = 0x5c;
        canvas[i * 4 + 3] = 0xFF;
    }
    let mut y = pad;
    for (name, shape) in all {
        let out = render::draw_pill(w, h, scale, &clock, &[weather.clone()], None, &[], None, shape);
        blit(&mut canvas, cw, &out.rgba, w, h, pad, y);
        println!("  {name}: shape={shape:?}");
        y += h + gap;
    }
    shot("shapes", scale, cw, ch, &canvas);
    render::shutdown(token);
}
