//! GDI+ 残留能力：程序/托盘/封面图标捕获与缩放、文本测量（面板尺寸预算）、
//! 旋转封面像素运算，以及分层窗口上屏（premultiplied BGRA → UpdateLayeredWindow）。
//! UI 绘制已全部迁到 egui 软件后端（见 egui_soft / pillui / dockview / widgetbarview /
//! musicbarview / panelview）；本文件只保留非绘制的图像处理与 GDI+ 文本度量。

#![allow(non_snake_case)]

use std::ptr::null_mut;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetDIBits, GetObjectW,
    ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::Graphics::GdiPlus::{
    BitmapData, FontStyleBold, GdiplusShutdown, GdiplusStartup, GdiplusStartupInput,
    GdipBitmapLockBits, GdipBitmapUnlockBits, GdipCreateBitmapFromScan0, GdipCreateBitmapFromStream,
    GdipCreateFont, GdipCreateFontFamilyFromName, GdipDeleteFont, GdipDeleteFontFamily,
    GdipDeleteGraphics, GdipDeleteStringFormat, GdipDisposeImage, GdipGetImageGraphicsContext,
    GdipGetImageHeight, GdipGetImageWidth, GdipMeasureString, GdipStringFormatGetGenericDefault,
    GpBitmap, GpFont, GpFontFamily, GpGraphics, ImageLockModeRead, Rect as GpRect, RectF, UnitPixel,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, UpdateLayeredWindow, DI_NORMAL, HICON, ULW_ALPHA,
};

use crate::config as cfg;

/// GDI+ 未导出的像素格式常量（GdiPlusPixels.h 的 PixelFormat32bppARGB）
const PX_32BPP_ARGB: i32 = 0x0026_200A;

// ---------- 生命周期 ----------

/// 启动 GDI+，返回 token（退出时传给 shutdown）。
pub fn startup() -> usize {
    let mut token = 0usize;
    let input = GdiplusStartupInput {
        GdiplusVersion: 1,
        DebugEventCallback: 0,
        SuppressBackgroundThread: false.into(),
        SuppressExternalCodecs: false.into(),
    };
    unsafe { GdiplusStartup(&mut token, &input, null_mut()) };
    token
}

pub fn shutdown(token: usize) {
    unsafe { GdiplusShutdown(token) };
}

/// 把 premultiplied BGRA 画面上屏（分层窗口逐像素 alpha）。两个窗口共用。
pub fn present(hwnd: HWND, bgra: &[u8], w: i32, h: i32) {
    unsafe {
        let hdc = GetDC(hwnd);
        let mem = CreateCompatibleDC(hdc);
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = 0; // BI_RGB
        let mut bits: *mut core::ffi::c_void = null_mut();
        let Ok(hbmp) = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(mem);
            ReleaseDC(hwnd, hdc);
            return;
        };
        let old = SelectObject(mem, hbmp);
        if !bits.is_null() {
            std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits as *mut u8, bgra.len());
        }
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let size = SIZE { cx: w, cy: h };
        let _ = UpdateLayeredWindow(
            hwnd, hdc, None, Some(&size), mem, Some(&windows::Win32::Foundation::POINT { x: 0, y: 0 }),
            COLORREF(0), Some(&blend), ULW_ALPHA,
        );
        let _ = SelectObject(mem, old);
        let _ = DeleteObject(hbmp);
        let _ = DeleteDC(mem);
        ReleaseDC(hwnd, hdc);
    }
}

// ---------- 文本测量 ----------

/// 字体族：正文中文、符号图标
#[derive(Clone, Copy, PartialEq)]
pub enum FontKind {
    Cjk,
    Symbol,
}

/// 测量 UTF-16 文本的宽高（物理像素）。
pub fn measure_text(text: &[u16], size: f32, kind: FontKind, scale: f32) -> (f32, f32) {
    unsafe {
        let Some(g) = scratch_graphics() else { return (0.0, 0.0) };
        let mut font = null_mut();
        if !create_font(kind, size * scale, &mut font) {
            GdipDeleteGraphics(g);
            return (0.0, 0.0);
        }
        let mut fmt = null_mut();
        GdipStringFormatGetGenericDefault(&mut fmt);
        let layout = RectF { X: 0.0, Y: 0.0, Width: 2000.0, Height: 400.0 };
        let mut box_ = RectF { X: 0.0, Y: 0.0, Width: 0.0, Height: 0.0 };
        GdipMeasureString(
            g, PCWSTR(text.as_ptr()), text.len() as i32, font, &layout, fmt,
            &mut box_, null_mut(), null_mut(),
        );
        GdipDeleteStringFormat(fmt);
        GdipDeleteFont(font);
        GdipDeleteGraphics(g);
        (box_.Width, box_.Height)
    }
}

// ---------- 胶囊绘制 ----------

/// 胶囊右侧的一个托盘闪烁图标（新消息提醒）：图标位图 + 这一帧是否在"灭"。
/// 灭帧画全透明（格子仍占位），亮帧画真实图标——与 Dock 托盘区的闪烁同一节奏。
pub struct FlashView<'a> {
    pub rgba: &'a [u8],
    pub dim: bool,
}

/// 胶囊内的一个小 widget：一个符号图标 + 一段短文本
#[derive(Clone)]
pub struct WidgetItem {
    pub icon: String,
    pub icon_argb: u32,
    pub text: String,
    /// 图标画法：0 = 用 `icon` 里的字形，1 = 信封，2 = 剪贴板
    /// （1/2 是矢量绘制，见 `egui_soft::widget_icon`；`icon` 仅作布局占位标识）
    pub icon_kind: u8,
}

fn to_u16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// 胶囊形状（决定四角圆角与命中测试，见 `settings::PillAnchor`）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PillShape {
    /// 悬浮胶囊：四角全圆（半径 = 高/2）
    Float,
    /// 吸顶刘海：顶部两角向外张开（耳），底部半圆
    Island,
}

// ---------- 刘海耳（吸顶 / 吸底的边缘过渡） ----------

/// 吸顶刘海 / 吸底模式下，贴边那条边两端的"耳朵"尺寸：(宽, 高)，逻辑点，
/// 随条高等比。耳朵把贴边的方角换成一段向外张开的凹弧：剪影沿屏幕边缘
/// 多探出 `宽`，再用 `高` 的深度平滑收回条身，接缝不再是一个生硬的直角
/// （参考 Dynamic Island / 刘海与屏幕顶边的过渡）。
pub fn ear_size(fh: f32) -> (f32, f32) {
    ((fh * 0.16).max(2.0), (fh * 0.22).max(3.0))
}

/// 当前胶囊模式下的耳朵物理像素宽（悬浮模式没有耳朵，返回 0）。
/// 贴边模式的窗口比条身左右各宽一只耳朵，音乐条 / 模块条定位时用它补偿，
/// 让"视觉间隙"仍然按条身算。
pub fn ear_w_phys(scale: f32) -> i32 {
    match crate::settings::pill_anchor() {
        crate::settings::PillAnchor::Float => 0,
        _ => (ear_size(cfg::PILL_H).0 * scale).round() as i32,
    }
}
// ---------- 音乐条（旋转封面 + 歌词） ----------

/// 封面边长（物理像素）
pub fn cover_px(scale: f32) -> i32 {
    (cfg::MUSIC_COVER * scale).round().max(8.0) as i32
}

/// 把播放器给的封面字节（JPEG/PNG…）解码成 px×px 的正方形 RGBA：
/// 解码 → 中心裁切 → 面积平均缩放。任何一步失败都返回 None。
pub fn decode_cover(bytes: &[u8], px: i32) -> Option<Vec<u8>> {
    if bytes.is_empty() || px <= 0 {
        return None;
    }
    unsafe {
        let stream = SHCreateMemStream(Some(bytes))?;
        let mut bmp: *mut GpBitmap = null_mut();
        if GdipCreateBitmapFromStream(&stream, &mut bmp).0 != 0 || bmp.is_null() {
            return None;
        }
        let mut iw = 0u32;
        let mut ih = 0u32;
        GdipGetImageWidth(bmp as _, &mut iw);
        GdipGetImageHeight(bmp as _, &mut ih);
        if iw == 0 || ih == 0 {
            GdipDisposeImage(bmp as _);
            return None;
        }
        let rect = GpRect { X: 0, Y: 0, Width: iw as i32, Height: ih as i32 };
        let mut data = BitmapData::default();
        if GdipBitmapLockBits(bmp, &rect, ImageLockModeRead.0 as u32, PX_32BPP_ARGB, &mut data).0 != 0
            || data.Scan0.is_null()
        {
            let _ = GdipBitmapUnlockBits(bmp, &mut data);
            GdipDisposeImage(bmp as _);
            return None;
        }
        // 中心裁成正方形（封面可能是 16:9 的 MV 截帧）
        let side = iw.min(ih) as usize;
        let (ox, oy) = ((iw as usize - side) / 2, (ih as usize - side) / 2);
        let stride = data.Stride as usize;
        let mut sq = vec![0u8; side * side * 4];
        let base = data.Scan0 as *const u8;
        for y in 0..side {
            let src = base.add((y + oy) * stride + ox * 4);
            std::ptr::copy_nonoverlapping(src, sq.as_mut_ptr().add(y * side * 4), side * 4);
        }
        let _ = GdipBitmapUnlockBits(bmp, &mut data);
        GdipDisposeImage(bmp as _);
        swap_rb(&mut sq);
        Some(resize_rgba(&sq, side as i32, side as i32, px, px))
    }
}

/// 把正方形封面按角度旋转成圆形（straight RGBA，px*px*4）：
/// 逐像素逆旋转采样，边缘 1px 抗锯齿，圆外全透明。专辑封面本来就该转起来。
pub fn rotate_cover(src: &[u8], px: i32, deg: f32) -> Vec<u8> {
    let n = px.max(1) as usize;
    let mut out = vec![0u8; n * n * 4];
    if src.len() < n * n * 4 {
        return out;
    }
    let c = n as f32 / 2.0;
    let (sin, cos) = deg.to_radians().sin_cos();
    for y in 0..n {
        for x in 0..n {
            let dx = x as f32 + 0.5 - c;
            let dy = y as f32 + 0.5 - c;
            let d = (dx * dx + dy * dy).sqrt();
            if d > c {
                continue;
            }
            // 逆旋转回原图坐标（旋转不会改变到圆心的距离，所以源点必在图内）
            let sx = c + dx * cos + dy * sin - 0.5;
            let sy = c - dx * sin + dy * cos - 0.5;
            let (r, g, b) = sample_rgb(src, n, sx, sy);
            let a = ((c - d).clamp(0.0, 1.0) * 255.0).round() as u8;
            let i = (y * n + x) * 4;
            out[i] = r;
            out[i + 1] = g;
            out[i + 2] = b;
            out[i + 3] = a;
        }
    }
    out
}

/// 双线性采样（straight RGBA，只取 RGB；封面视为不透明）
fn sample_rgb(src: &[u8], n: usize, x: f32, y: f32) -> (u8, u8, u8) {
    let max = n as f32 - 1.0;
    let x = x.clamp(0.0, max);
    let y = y.clamp(0.0, max);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(n - 1), (y0 + 1).min(n - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let px = |xx: usize, yy: usize, ch: usize| src[(yy * n + xx) * 4 + ch] as f32;
    let mut out = [0u8; 3];
    for (c, slot) in out.iter_mut().enumerate() {
        let top = px(x0, y0, c) * (1.0 - fx) + px(x1, y0, c) * fx;
        let bot = px(x0, y1, c) * (1.0 - fx) + px(x1, y1, c) * fx;
        *slot = (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
    }
    (out[0], out[1], out[2])
}

// ---------- 展开面板绘制 ----------

/// 面板里的一行
pub struct PanelRow {
    pub icon: String,
    pub icon_argb: u32,
    pub title: String,
    pub sub: String,
    pub value: String,
    pub value_argb: u32,
    /// Some(true)=已固定 Some(false)=未固定 None=这一行没有固定按钮
    pub pin: Option<bool>,
}

/// 行体（复制/打开）的动作号基数；固定按钮 = PIN_BASE + 行号；标签页 = TAB_BASE + 序号
pub const PIN_BASE: u32 = 1000;
pub const TAB_BASE: u32 = 2000;

fn panel_row_width(row: &PanelRow, scale: f32) -> f32 {
    let mut w = 0.0;
    if !row.icon.is_empty() {
        w += measure_text(&to_u16(&row.icon), cfg::WIDGET_ICON_SIZE, FontKind::Symbol, scale).0
            + cfg::PANEL_ICON_GAP * scale;
    }
    let t = measure_text(&to_u16(&row.title), cfg::PANEL_ROW_TITLE, FontKind::Cjk, scale).0;
    let s = if row.sub.is_empty() {
        0.0
    } else {
        measure_text(&to_u16(&row.sub), cfg::PANEL_ROW_SUB, FontKind::Cjk, scale).0
    };
    w += t.max(s);
    if !row.value.is_empty() {
        w += cfg::PANEL_VALUE_GAP * scale
            + measure_text(&to_u16(&row.value), cfg::PANEL_VALUE_SIZE, FontKind::Cjk, scale).0;
    }
    if row.pin.is_some() {
        w += cfg::PIN_W * scale;
    }
    w
}

/// 标签页按钮的宽度
fn tab_width(text: &str, scale: f32) -> f32 {
    measure_text(&to_u16(text), cfg::PANEL_SUB_SIZE, FontKind::Cjk, scale).0 + 18.0 * scale
}

fn tabs_width(tabs: &[String], scale: f32) -> f32 {
    if tabs.is_empty() {
        return 0.0;
    }
    let mut w = 0.0;
    for t in tabs {
        w += tab_width(t, scale) + 4.0 * scale;
    }
    w
}

/// 面板尺寸（与 draw_panel 布局严格一致）
pub fn panel_size(
    scale: f32, title: &str, subtitle: &str, rows: &[PanelRow], empty: &str,
    tabs: &[String],
) -> (i32, i32) {
    let pad = cfg::PANEL_PAD * scale;
    let head = measure_text(&to_u16(title), cfg::PANEL_TITLE_SIZE, FontKind::Cjk, scale).0.max(
        measure_text(&to_u16(subtitle), cfg::PANEL_SUB_SIZE, FontKind::Cjk, scale).0,
    ) + 16.0 * scale
        + tabs_width(tabs, scale);
    let mut inner = head;
    for r in rows {
        inner = inner.max(panel_row_width(r, scale));
    }
    if rows.is_empty() {
        inner = inner.max(measure_text(&to_u16(empty), cfg::PANEL_ROW_SUB, FontKind::Cjk, scale).0);
    }
    let head_h = cfg::PANEL_HEAD_H - if subtitle.is_empty() { 12.0 } else { 0.0 };
    let body_h = if rows.is_empty() {
        cfg::PANEL_ROW_H
    } else {
        rows.len() as f32 * cfg::PANEL_ROW_H
    };
    let w = (inner + pad * 2.0).clamp(cfg::PANEL_MIN_W * scale, cfg::PANEL_MAX_W * scale);
    let h = head_h * scale + body_h * scale + pad;
    (w.ceil() as i32, h.ceil() as i32)
}

// ---------- 天气面板（宽卡片）----------

pub struct WeatherDay {
    pub label: String,
    pub icon: String,
    pub icon_argb: u32,
    pub high: String,
    pub low: String,
    /// 白天 / 夜间天气，例如 "晴 / 多云"
    pub text: String,
    /// 末行补充信息（风力或降水概率）
    pub note: String,
    pub note_argb: u32,
}

pub struct WeatherView {
    pub city: String,
    pub temp: String,
    pub condition: String,
    pub icon: String,
    pub icon_argb: u32,
    /// 实况补充信息，最多两行，例如 ["体感 6° · 湿度 58%", "东北风 3 级"]
    pub details: Vec<String>,
    /// "高德 · 17:30 发布"
    pub report: String,
    pub date: String,
    pub time: String,
    pub days: Vec<WeatherDay>,
}

/// 左栏（实况块）的纵向节奏（逻辑单位，绘制与定高共用同一套数值）：
/// 图标/温度行 → 天气·城市行 → 详情（每行 18）→ 来源。最后一项是来源行的字高。
fn left_group_height(detail_lines: usize, k: f32) -> f32 {
    let n = detail_lines.min(2) as f32;
    (LEFT_DET_DY + n * LEFT_DET_STEP + LEFT_REP_GAP + 13.0) * k
}

// 左栏各元素相对块顶部的偏移（逻辑单位，会再乘字号倍率）
const LEFT_DET_DY: f32 = 84.0;
const LEFT_DET_STEP: f32 = 18.0;
const LEFT_REP_GAP: f32 = 6.0;

/// 卡片尺寸：基准尺寸随字号倍率长大；左栏更高时再按需撑高，绝不裁字。
pub fn weather_panel_size(scale: f32, v: &WeatherView) -> (i32, i32) {
    let k = crate::settings::weather_text_scale();
    let base_w = cfg::WEATHER_PANEL_W + 480.0 * (k - 1.0);
    // 高度下限照顾右栏（四天）的竖向占用，左栏更多行时再由 need_h 撑高
    let base_h = cfg::WEATHER_PANEL_H * 0.88 * k;
    // 上边距 34 + 左栏 + 下边距 16
    let need_h = 34.0 * k + left_group_height(v.details.len(), k) + 16.0;
    (
        (base_w * scale).ceil() as i32,
        (base_h.max(need_h) * scale).ceil() as i32,
    )
}

// ---------- 图标捕获 ----------

/// 窗口图标兜底链：WM_GETICON(SMALL/BIG) → 类图标 SM/BIG
pub unsafe fn window_icon(hwnd: HWND) -> Option<HICON> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassLongPtrW, SendMessageW, GCLP_HICON, GCLP_HICONSM, WM_GETICON,
    };
    for size in [0usize /*ICON_SMALL*/, 1 /*ICON_BIG*/] {
        let h = SendMessageW(hwnd, WM_GETICON, windows::Win32::Foundation::WPARAM(size), windows::Win32::Foundation::LPARAM(0)).0;
        if h != 0 {
            return Some(HICON(h as *mut _));
        }
    }
    for gcl in [GCLP_HICONSM, GCLP_HICON] {
        let h = GetClassLongPtrW(hwnd, gcl);
        if h != 0 {
            return Some(HICON(h as *mut _));
        }
    }
    None
}

/// HICON 里那张位图的原始边长（正方取较大边）。拿不到返回 None。
///
/// 用途：先把图标按**原始尺寸**抓成像素，再用我们自己的重采样缩放到目标尺寸。
/// 直接让 `DrawIconEx` 画到目标尺寸时，GDI 会用最快的插值放大 16px 的托盘图标，
/// 放大后边缘发虚、颜色发灰——这正是"托盘图标放大后不清晰"的根因。
pub fn icon_native_size(hicon: HICON) -> Option<i32> {
    use windows::Win32::Graphics::Gdi::{GetObjectW, BITMAP, HGDIOBJ};
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};
    unsafe {
        let mut info = ICONINFO::default();
        if GetIconInfo(hicon, &mut info).is_err() {
            return None;
        }
        let measure = |hbmp: windows::Win32::Graphics::Gdi::HBITMAP| -> i32 {
            if hbmp.is_invalid() {
                return 0;
            }
            let mut bmp = BITMAP::default();
            let n = GetObjectW(
                HGDIOBJ(hbmp.0),
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bmp as *mut _ as *mut core::ffi::c_void),
            );
            if n > 0 {
                bmp.bmWidth.max(bmp.bmHeight)
            } else {
                0
            }
        };
        // 彩色位图优先；单色图标只有 mask（尺寸同样有效）
        let px = measure(info.hbmColor).max(measure(info.hbmMask));
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(info.hbmColor);
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(info.hbmMask);
        }
        (px > 0).then_some(px)
    }
}

/// premultiplied BGRA（DIB 内存序）→ straight-alpha RGBA。
/// HICON 绘制出的 DIB 和 `IShellItemImageFactory` 给的位图都是这个格式，共用一套拆包。
fn bgra_to_rgba(bytes: &[u8]) -> Vec<u8> {
    let mut rgba = vec![0u8; bytes.len()];
    for (i, px) in bytes.chunks_exact(4).enumerate() {
        let (b, g, r_, a) = (px[0] as u32, px[1] as u32, px[2] as u32, px[3] as u32);
        let (j0, j1, j2, j3) = (i * 4, i * 4 + 1, i * 4 + 2, i * 4 + 3);
        if a == 0 {
            continue;
        }
        if a == 255 {
            rgba[j0] = r_ as u8;
            rgba[j1] = g as u8;
            rgba[j2] = b as u8;
            rgba[j3] = 255;
        } else {
            rgba[j0] = ((r_ * 255 + a / 2) / a) as u8;
            rgba[j1] = ((g * 255 + a / 2) / a) as u8;
            rgba[j2] = ((b * 255 + a / 2) / a) as u8;
            rgba[j3] = a as u8;
        }
    }
    rgba
}

/// 把任意进程的 HICON 画进 32bpp DIB 并转成 straight-alpha RGBA（边长 = px）。
/// HICON 是跨进程共享的 USER 对象，可直接在自己进程绘制。
pub fn capture_icon_rgba(hicon: HICON, px: i32) -> Option<Vec<u8>> {
    unsafe {
        let hdc = GetDC(HWND::default());
        let mem = CreateCompatibleDC(hdc);
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: px,
            biHeight: -px,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = null_mut();
        let Ok(hbmp) = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(mem);
            ReleaseDC(HWND::default(), hdc);
            return None;
        };
        let old = SelectObject(mem, hbmp);
        let ok = DrawIconEx(mem, 0, 0, hicon, px, px, 0, None, DI_NORMAL).is_ok();
        let _ = SelectObject(mem, old);
        let mut out = None;
        if ok && !bits.is_null() {
            let bytes = std::slice::from_raw_parts(bits as *const u8, (px * px * 4) as usize);
            out = Some(bgra_to_rgba(bytes));
        }
        let _ = DeleteObject(hbmp);
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), hdc);
        out
    }
}

/// 读一张 32bpp 位图（`IShellItemImageFactory::GetImage` 给的应用图标就是 HBITMAP），
/// 转成 straight-alpha RGBA 并缩到边长 px。读完就地释放位图，调用方不用管生命周期。
///
/// 位图实际尺寸要问 `GetObjectW`：`SIIGBF_BIGGERSIZEOK` 允许外壳返回比请求更大的图
/// （经常是 256px），照着请求尺寸硬读只会读到左上角一块。
pub fn bitmap_rgba(hbm: HBITMAP, px: i32) -> Option<Vec<u8>> {
    if px <= 0 || hbm.is_invalid() {
        return None;
    }
    unsafe {
        let mut bm = BITMAP::default();
        let got = GetObjectW(
            hbm,
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut core::ffi::c_void),
        );
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        if got == 0 || w <= 0 || h <= 0 {
            let _ = DeleteObject(hbm);
            return None;
        }
        let hdc = GetDC(HWND::default());
        let mem = CreateCompatibleDC(hdc);
        let old = SelectObject(mem, hbm);
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0,
            ..Default::default()
        };
        let mut bytes = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(
            mem,
            hbm,
            0,
            h as u32,
            Some(bytes.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        let _ = SelectObject(mem, old);
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), hdc);
        let _ = DeleteObject(hbm);
        if lines == 0 {
            return None;
        }
        let rgba = bgra_to_rgba(&bytes);
        if w == px && h == px {
            return Some(rgba);
        }
        Some(resize_rgba(&rgba, w, h, px, px))
    }
}

/// 高质量缩小：面积平均（按 alpha 加权），用于把 256px 系统图标缩到 Dock 尺寸。
pub fn resize_rgba(src: &[u8], sw: i32, sh: i32, dw: i32, dh: i32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    if sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0 {
        return out;
    }
    let xr = sw as f32 / dw as f32;
    let yr = sh as f32 / dh as f32;
    for dy in 0..dh {
        let y0 = (dy as f32 * yr) as i32;
        let y1 = (((dy + 1) as f32 * yr) as i32).clamp(y0 + 1, sh);
        for dx in 0..dw {
            let x0 = (dx as f32 * xr) as i32;
            let x1 = (((dx + 1) as f32 * xr) as i32).clamp(x0 + 1, sw);
            let (mut r, mut g, mut b, mut a, mut n) = (0f32, 0f32, 0f32, 0f32, 0f32);
            for sy in y0..y1 {
                let row = (sy * sw) as usize * 4;
                for sx in x0..x1 {
                    let i = row + sx as usize * 4;
                    let sa = src[i + 3] as f32;
                    r += src[i] as f32 * sa;
                    g += src[i + 1] as f32 * sa;
                    b += src[i + 2] as f32 * sa;
                    a += sa;
                    n += 1.0;
                }
            }
            let di = ((dy * dw + dx) * 4) as usize;
            if a > 0.0 {
                out[di] = (r / a).round() as u8;
                out[di + 1] = (g / a).round() as u8;
                out[di + 2] = (b / a).round() as u8;
                out[di + 3] = (a / n).round() as u8;
            }
        }
    }
    out
}

/// GDI+ 位图的字节序是 BGRA；本模块内部统一用 straight-alpha RGBA。
fn swap_rb(buf: &mut [u8]) {
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
}

/// premultiplied RGBA → premultiplied BGRA（UpdateLayeredWindow 要求）。
///
/// 只换字节序，不做任何透明度处理：「界面透明度」只作用在**板面底色**上
/// （`config::bg_alpha`），文字和图标保持原样，这样滑块拉低时只有背景透出桌面。
pub fn premultiplied_rgba_to_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for (i, chunk) in rgba.chunks_exact(4).enumerate() {
        out[i * 4] = chunk[2];
        out[i * 4 + 1] = chunk[1];
        out[i * 4 + 2] = chunk[0];
        out[i * 4 + 3] = chunk[3];
    }
    out
}

// ---------- 内部工具 ----------

unsafe fn scratch_graphics() -> Option<*mut GpGraphics> {
    let mut bmp: *mut GpBitmap = null_mut();
    GdipCreateBitmapFromScan0(16, 16, 64, PX_32BPP_ARGB, None, &mut bmp);
    if bmp.is_null() { return None; }
    let mut g: *mut GpGraphics = null_mut();
    GdipGetImageGraphicsContext(bmp as _, &mut g);
    GdipDisposeImage(bmp as _);
    if g.is_null() { None } else { Some(g) }
}

/// 字体在 px 字号下、基线以上部分的高度（物理像素）。DrawString 以行框顶部对齐
/// layout.Y，基线到行框顶 ≈ ascent。胶囊里所有文字/图标改用**统一基线**对齐：
/// 各自按行盒垂直居中时，Latin/CJK/Symbol 的行框上下留白比例不同，
/// 会出现"有高有低"的视觉错位；共基线后所有文字落在同一条线上。
unsafe fn create_font(kind: FontKind, px: f32, font: &mut *mut GpFont) -> bool {
    let name: PCWSTR = match kind {
        FontKind::Cjk => w!("Microsoft YaHei UI"),
        FontKind::Symbol => w!("Segoe UI Symbol"),
    };
    let mut family: *mut GpFontFamily = null_mut();
    GdipCreateFontFamilyFromName(name, null_mut(), &mut family);
    if family.is_null() {
        GdipCreateFontFamilyFromName(w!("Segoe UI"), null_mut(), &mut family);
    }
    if family.is_null() { return false; }
    // 符号图标用常规字重，文字统一粗体
    let style = if kind == FontKind::Symbol { 0 } else { FontStyleBold.0 };
    GdipCreateFont(family, px, style, UnitPixel, font);
    GdipDeleteFontFamily(family);
    !font.is_null()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_swap_keeps_alpha_and_colors() {
        // 预乘 RGBA（R,G,B,A）→ BGRA：只换字节序，透明度不参与（透明度只作用于板面底色）
        assert_eq!(
            premultiplied_rgba_to_bgra(&[100, 50, 25, 200]),
            vec![25, 50, 100, 200]
        );
        assert_eq!(premultiplied_rgba_to_bgra(&[10, 20, 30, 40]), vec![30, 20, 10, 40]);
    }
}
