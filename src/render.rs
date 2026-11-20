//! GDI+ 绘制：胶囊圆角底板、文本、分隔线、悬停背景；
//! 托盘图标的像素捕获；premultiplied BGRA 转换（供 UpdateLayeredWindow）。
//! 全部输出 32bpp 逐像素 alpha，无任何系统级持久修改。

#![allow(non_snake_case)]

use std::ptr::null_mut;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, DIB_RGB_COLORS,
};
use windows::Win32::Graphics::GdiPlus::{
    GdipAddPathArc, GdipAddPathPolygon, GdipBitmapLockBits, GdipBitmapUnlockBits, GdipClosePathFigure,
    GdipCreateBitmapFromScan0, GdipCreateBitmapFromStream, GdipCreateFont, GdipCreateFontFamilyFromName,
    GdipCreatePath, GdipCreatePen1, GdipCreateSolidFill, GdipDeleteBrush, GdipDeleteFont,
    GdipDeleteFontFamily, GdipDeleteGraphics, GdipDeletePath, GdipDeletePen, GdipDeleteStringFormat,
    GdipDisposeImage, GdipDrawEllipse, GdipDrawImageRectI, GdipDrawPath, GdipDrawString, GdipFillEllipse,
    GdipFillEllipseI, GdipFillPath, GdipFillRectangleI, GdipGetImageGraphicsContext,
    GdipGetImageHeight, GdipGetImageWidth, GdipMeasureString, GdipSetInterpolationMode,
    GdipSetSmoothingMode, GdipSetTextRenderingHint, GdipStringFormatGetGenericDefault,
    GdiplusShutdown, GdiplusStartup, GdiplusStartupInput, BitmapData, FillModeWinding, FontStyleBold,
    GpBitmap, GpBrush, GpFont, GpFontFamily, GpGraphics, GpPath, GpSolidFill,
    ImageLockModeRead, InterpolationModeHighQualityBicubic, PointF, Rect as GpRect, RectF,
    SmoothingModeAntiAlias, TextRenderingHintAntiAliasGridFit, UnitPixel,
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

/// 字体族：正文拉丁、中文回退、符号图标
#[derive(Clone, Copy, PartialEq)]
pub enum FontKind {
    Latin,
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

pub struct PillRender {
    /// 整幅 straight-alpha RGBA 缓冲（w*h*4）
    pub rgba: Vec<u8>,
    /// 每个 widget 的命中矩形（client 坐标 [x, y, w, h]）
    pub widget_rects: Vec<[f32; 4]>,
}

/// 胶囊内的一个小 widget：一个符号图标 + 一段短文本
pub struct WidgetItem {
    pub icon: String,
    pub icon_argb: u32,
    pub text: String,
}

fn to_u16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// widget 内容宽度（图标 + 间距 + 文本）
pub fn widget_width(item: &WidgetItem, scale: f32) -> f32 {
    let mut w = 0.0;
    if !item.icon.is_empty() {
        w += measure_text(&to_u16(&item.icon), cfg::WIDGET_ICON_SIZE, FontKind::Symbol, scale).0;
        if !item.text.is_empty() {
            w += cfg::WIDGET_ICON_GAP * scale;
        }
    }
    if !item.text.is_empty() {
        w += measure_text(&to_u16(&item.text), cfg::WIDGET_TEXT_SIZE, FontKind::Cjk, scale).0;
    }
    w
}

/// 绘制整幅胶囊。w/h 为物理像素。
pub fn draw_pill(
    w: i32, h: i32, scale: f32,
    time: &[u16], date: &[u16],
    widgets: &[WidgetItem],
    hovered_widget: Option<usize>,
) -> PillRender {
    unsafe {
        let mut bmp: *mut GpBitmap = null_mut();
        GdipCreateBitmapFromScan0(w, h, w * 4, PX_32BPP_ARGB, None, &mut bmp);
        if bmp.is_null() {
            return PillRender { rgba: vec![0; (w * h * 4) as usize], widget_rects: vec![] };
        }
        let mut g: *mut GpGraphics = null_mut();
        GdipGetImageGraphicsContext(bmp as _, &mut g);
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
        GdipSetTextRenderingHint(g, TextRenderingHintAntiAliasGridFit);

        // 圆角底板（半径 = 高/2，完整胶囊形）
        let r = h as f32 / 2.0;
        let (fw, fh) = (w as f32, h as f32);
        let mut path: *mut GpPath = null_mut();
        GdipCreatePath(FillModeWinding, &mut path);
        GdipAddPathArc(path, 0.0, 0.0, 2.0 * r, 2.0 * r, 180.0, 90.0);
        GdipAddPathArc(path, fw - 2.0 * r, 0.0, 2.0 * r, 2.0 * r, 270.0, 90.0);
        GdipAddPathArc(path, fw - 2.0 * r, fh - 2.0 * r, 2.0 * r, 2.0 * r, 0.0, 90.0);
        GdipAddPathArc(path, 0.0, fh - 2.0 * r, 2.0 * r, 2.0 * r, 90.0, 90.0);
        GdipClosePathFigure(path);
        let bg = solid_fill(cfg::BG_ARGB);
        GdipFillPath(g, bg as *mut GpBrush, path);
        GdipDeleteBrush(bg as *mut GpBrush);
        GdipDeletePath(path);

        // 文本布局（与 content_width 严格一致）
        let pad = cfg::PAD_X * scale;
        let gap = cfg::GAP * scale;
        let (tw, th) = measure_text(time, cfg::TIME_SIZE, FontKind::Latin, scale);
        let (dw, dh) = measure_text(date, cfg::DATE_SIZE, FontKind::Cjk, scale);
        let mut x = pad;
        draw_string(g, time, cfg::TIME_ARGB, FontKind::Latin, cfg::TIME_SIZE * scale, x, (fh - th) / 2.0, fw, fh);
        x += tw + gap;
        draw_string(g, date, cfg::DATE_ARGB, FontKind::Cjk, cfg::DATE_SIZE * scale, x, (fh - dh) / 2.0, fw, fh);
        x += dw;

        let bg_px = cfg::ICON_BG * scale;
        let div_y = ((fh - cfg::DIVIDER_H * scale) / 2.0) as i32;
        let div_h = (cfg::DIVIDER_H * scale) as i32;
        let mut widget_rects = Vec::new();

        // widget 区（天气 / 音乐 / 剪贴板）
        if !widgets.is_empty() {
            x += gap;
            let div = solid_fill(cfg::DIVIDER_ARGB);
            GdipFillRectangleI(g, div as *mut GpBrush, x as i32, div_y, 1, div_h);
            GdipDeleteBrush(div as *mut GpBrush);
            x += 1.0 + gap + cfg::WIDGET_PAD_X * scale;

            for (i, item) in widgets.iter().enumerate() {
                let ww = widget_width(item, scale);
                let ry = (fh - bg_px) / 2.0;
                let hw = ww + 2.0 * cfg::WIDGET_PAD_X * scale;
                if Some(i) == hovered_widget {
                    round_rect(g, x, ry, hw, bg_px, 8.0 * scale, cfg::HOVER_ARGB);
                }
                widget_rects.push([x, ry, hw, bg_px]);

                let mut cx = x + cfg::WIDGET_PAD_X * scale;
                if !item.icon.is_empty() {
                    let icon = to_u16(&item.icon);
                    let (iw, ih) = measure_text(&icon, cfg::WIDGET_ICON_SIZE, FontKind::Symbol, scale);
                    draw_string(g, &icon, item.icon_argb, FontKind::Symbol,
                        cfg::WIDGET_ICON_SIZE * scale, cx, (fh - ih) / 2.0, fw, fh);
                    cx += iw + cfg::WIDGET_ICON_GAP * scale;
                }
                if !item.text.is_empty() {
                    let text = to_u16(&item.text);
                    let (_, theight) = measure_text(&text, cfg::WIDGET_TEXT_SIZE, FontKind::Cjk, scale);
                    draw_string(g, &text, cfg::TEXT_ARGB, FontKind::Cjk,
                        cfg::WIDGET_TEXT_SIZE * scale, cx, (fh - theight) / 2.0, fw, fh);
                }
                x += hw + gap;
            }
        }

        // 取出像素
        let rect = GpRect { X: 0, Y: 0, Width: w, Height: h };
        let mut data = BitmapData::default();
        GdipBitmapLockBits(bmp, &rect, ImageLockModeRead.0 as u32, PX_32BPP_ARGB, &mut data);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        if !data.Scan0.is_null() {
            std::ptr::copy_nonoverlapping(data.Scan0 as *const u8, rgba.as_mut_ptr(), rgba.len());
        }
        GdipBitmapUnlockBits(bmp, &mut data);
        // GDI+ 的 32bppARGB 在内存里是 BGRA，统一换成 RGBA
        swap_rb(&mut rgba);

        GdipDeleteGraphics(g);
        GdipDisposeImage(bmp as _);
        PillRender { rgba, widget_rects }
    }
}

/// 胶囊总宽度（与 draw_pill 的布局严格一致，调用方据此先定窗口尺寸）。
pub fn content_width(time_w: f32, date_w: f32, widgets: &[WidgetItem], scale: f32) -> f32 {
    let pad = cfg::PAD_X * scale;
    let gap = cfg::GAP * scale;
    let mut x = pad + time_w + gap + date_w;
    if !widgets.is_empty() {
        x += gap + 1.0 + gap + cfg::WIDGET_PAD_X * scale;
        for item in widgets {
            x += widget_width(item, scale) + 2.0 * cfg::WIDGET_PAD_X * scale + gap;
        }
        x -= gap;
    }
    x + pad
}

// ---------- 音乐条（旋转封面 + 歌词） ----------

/// 封面边长（物理像素）
pub fn cover_px(scale: f32) -> i32 {
    (cfg::MUSIC_COVER * scale).round().max(8.0) as i32
}

/// 音乐条里除歌词之外的固定宽度（左边距 + 封面 + 间距 + 右边距）
pub fn music_bar_fixed_w(scale: f32) -> f32 {
    (cfg::MUSIC_COVER_PAD + cfg::MUSIC_COVER + cfg::MUSIC_COVER_GAP + cfg::LYRIC_PAD_X) * scale
}

/// 音乐条总宽度（物理像素）：固定部分 + 歌词文本
pub fn music_bar_w(text: &str, scale: f32, font: f32) -> f32 {
    (music_bar_fixed_w(scale) + measure_text(&to_u16(text), font, FontKind::Cjk, scale).0).ceil()
}

/// 封面在条内的正方形位置与边长：(x, y, 边长)，相对窗口左上角。
/// 命中测试（播放/暂停按钮）与绘制共用同一套数值。
pub fn cover_box(scale: f32, h: i32) -> (f32, f32, f32) {
    let d = (cfg::MUSIC_COVER * scale).min(h as f32 - 6.0 * scale).max(8.0);
    (cfg::MUSIC_COVER_PAD * scale, (h as f32 - d) / 2.0, d)
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

/// 画整条音乐条：圆角深色底 + 左侧圆形封面（按 angle 旋转）+ 中心播放/暂停按钮 + 右侧歌词。
/// 没有封面时画一个暗色圆 + 音符占位。返回 straight-alpha RGBA。
pub fn draw_music_bar(
    w: i32, h: i32, scale: f32, text: &str, font: f32,
    cover: Option<&[u8]>, angle: f32, playing: bool, hovered: bool,
) -> Vec<u8> {
    if w <= 0 || h <= 0 {
        return Vec::new();
    }
    unsafe {
        let mut bmp: *mut GpBitmap = null_mut();
        GdipCreateBitmapFromScan0(w, h, w * 4, PX_32BPP_ARGB, None, &mut bmp);
        if bmp.is_null() {
            return vec![0; (w * h * 4) as usize];
        }
        let mut g: *mut GpGraphics = null_mut();
        GdipGetImageGraphicsContext(bmp as _, &mut g);
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
        GdipSetTextRenderingHint(g, TextRenderingHintAntiAliasGridFit);

        let (fw, fh) = (w as f32, h as f32);
        round_rect(g, 0.0, 0.0, fw, fh, fh / 2.0, cfg::BG_ARGB);

        // 封面（圆形，随角度旋转）
        let (cx0, cy0, d) = cover_box(scale, h);
        let (ccx, ccy) = (cx0 + d / 2.0, cy0 + d / 2.0);
        let mut drew_cover = false;
        if let Some(src) = cover.filter(|s| !s.is_empty()) {
            let px = d.round().max(8.0) as i32;
            let mut bgra = rotate_cover(src, px, angle);
            swap_rb(&mut bgra);
            let mut img: *mut GpBitmap = null_mut();
            GdipCreateBitmapFromScan0(px, px, px * 4, PX_32BPP_ARGB, Some(bgra.as_ptr()), &mut img);
            if !img.is_null() {
                GdipSetInterpolationMode(g, InterpolationModeHighQualityBicubic);
                GdipDrawImageRectI(g, img as _, cx0 as i32, cy0 as i32, d as i32, d as i32);
                GdipDisposeImage(img as _);
                drew_cover = true;
            }
        }
        if !drew_cover {
            let disc = solid_fill(0x26_FF_FF_FF);
            GdipFillEllipse(g, disc as *mut GpBrush, cx0, cy0, d, d);
            GdipDeleteBrush(disc as *mut GpBrush);
            let icon = to_u16(cfg::MUSIC_PLACEHOLDER);
            let size = cfg::LYRIC_ICON_SIZE * 1.7;
            let (iw, ih) = measure_text(&icon, size, FontKind::Symbol, scale);
            draw_string(g, &icon, cfg::ACCENT_ARGB, FontKind::Symbol, size * scale,
                ccx - iw / 2.0, ccy - ih / 2.0, fw, fh);
        }
        // 封面的细白描边：亮色封面压在深色条上也有一圈轮廓（占位圆同样适用）
        let mut pen: *mut windows::Win32::Graphics::GdiPlus::GpPen = null_mut();
        if GdipCreatePen1(0x40_FF_FF_FF, 1.0, UnitPixel, &mut pen).0 == 0 && !pen.is_null() {
            GdipDrawEllipse(g, pen, cx0 + 0.5, cy0 + 0.5, d - 1.0, d - 1.0);
            GdipDeletePen(pen);
        }

        // 中心按钮：半透明圆盘 + 播放/暂停图标（显示的是"点下去会发生什么"）
        let bd = d * cfg::MUSIC_BTN_RATIO;
        let btn_argb = if hovered { 0xE6_14_14_14 } else { 0xA6_10_10_10 };
        let btn = solid_fill(btn_argb);
        GdipFillEllipse(g, btn as *mut GpBrush, ccx - bd / 2.0, ccy - bd / 2.0, bd, bd);
        GdipDeleteBrush(btn as *mut GpBrush);
        // 图标直接画：34px 的封面里用字体排版会偏一两像素，手绘更稳
        if playing {
            // 暂停：两根竖条
            let bw = (bd * 0.15).max(2.0);
            let bh = bd * 0.44;
            let gapw = (bd * 0.14).max(1.0);
            let y = ccy - bh / 2.0;
            round_rect(g, ccx - gapw / 2.0 - bw, y, bw, bh, bw / 2.0, cfg::TIME_ARGB);
            round_rect(g, ccx + gapw / 2.0, y, bw, bh, bw / 2.0, cfg::TIME_ARGB);
        } else {
            // 播放：三角形（重心略向右偏，视觉上才居中）
            let half_h = bd * 0.22;
            let base_x = ccx - bd * 0.16;
            let tip_x = ccx + bd * 0.22;
            let pts = [
                PointF { X: base_x, Y: ccy - half_h },
                PointF { X: base_x, Y: ccy + half_h },
                PointF { X: tip_x, Y: ccy },
            ];
            let mut path: *mut GpPath = null_mut();
            GdipCreatePath(FillModeWinding, &mut path);
            GdipAddPathPolygon(path, pts.as_ptr(), pts.len() as i32);
            let brush = solid_fill(cfg::TIME_ARGB);
            GdipFillPath(g, brush as *mut GpBrush, path);
            GdipDeleteBrush(brush as *mut GpBrush);
            GdipDeletePath(path);
        }

        // 歌词（封面之后，也就是"控制在前、歌词在后"）
        let tx = cx0 + d + cfg::MUSIC_COVER_GAP * scale;
        let t = to_u16(text);
        let (_, th) = measure_text(&t, font, FontKind::Cjk, scale);
        draw_string(g, &t, cfg::TEXT_ARGB, FontKind::Cjk, font * scale,
            tx, (fh - th) / 2.0, fw - tx, fh);

        let rect = GpRect { X: 0, Y: 0, Width: w, Height: h };
        let mut data = BitmapData::default();
        GdipBitmapLockBits(bmp, &rect, ImageLockModeRead.0 as u32, PX_32BPP_ARGB, &mut data);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        if !data.Scan0.is_null() {
            std::ptr::copy_nonoverlapping(data.Scan0 as *const u8, rgba.as_mut_ptr(), rgba.len());
        }
        GdipBitmapUnlockBits(bmp, &mut data);
        swap_rb(&mut rgba);
        GdipDeleteGraphics(g);
        GdipDisposeImage(bmp as _);
        rgba
    }
}

// ---------- 悬停提示条 ----------

/// 悬停提示条：圆角底 + 一行文字。返回 (宽, 高, straight RGBA)。
/// 文字超宽时按省略号截断（托盘 tooltip 常带多行状态，这里只取首行）。
pub fn draw_tooltip(text: &str, scale: f32) -> (i32, i32, Vec<u8>) {
    let mut label = text.trim().to_string();
    if label.is_empty() {
        return (0, 0, Vec::new());
    }
    let max_w = cfg::TIP_MAX_W * scale;
    let mut chars: Vec<char> = label.chars().collect();
    let mut u = to_u16(&label);
    let (mut tw, mut th) = measure_text(&u, cfg::TIP_TEXT_SIZE, FontKind::Cjk, scale);
    while tw > max_w && chars.len() > 1 {
        chars.pop();
        label = format!("{}…", chars.iter().collect::<String>());
        u = to_u16(&label);
        let (w2, h2) = measure_text(&u, cfg::TIP_TEXT_SIZE, FontKind::Cjk, scale);
        tw = w2;
        th = th.max(h2);
    }
    if tw <= 0.0 {
        return (0, 0, Vec::new());
    }

    let pad_x = cfg::TIP_PAD_X * scale;
    let pad_y = cfg::TIP_PAD_Y * scale;
    let w = (tw + pad_x * 2.0).ceil() as i32;
    let h = (th + pad_y * 2.0).ceil() as i32;
    if w <= 0 || h <= 0 {
        return (0, 0, Vec::new());
    }

    unsafe {
        let mut bmp: *mut GpBitmap = null_mut();
        GdipCreateBitmapFromScan0(w, h, w * 4, PX_32BPP_ARGB, None, &mut bmp);
        if bmp.is_null() {
            return (0, 0, Vec::new());
        }
        let mut g: *mut GpGraphics = null_mut();
        GdipGetImageGraphicsContext(bmp as _, &mut g);
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
        GdipSetTextRenderingHint(g, TextRenderingHintAntiAliasGridFit);

        round_rect(g, 0.5, 0.5, w as f32 - 1.0, h as f32 - 1.0, cfg::TIP_RADIUS * scale, cfg::TIP_BG_ARGB);
        draw_string(
            g, &u, cfg::TIP_TEXT_ARGB, FontKind::Cjk,
            cfg::TIP_TEXT_SIZE * scale, pad_x, pad_y, w as f32, h as f32,
        );

        let rect = GpRect { X: 0, Y: 0, Width: w, Height: h };
        let mut data = BitmapData::default();
        GdipBitmapLockBits(bmp, &rect, ImageLockModeRead.0 as u32, PX_32BPP_ARGB, &mut data);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        if !data.Scan0.is_null() {
            std::ptr::copy_nonoverlapping(data.Scan0 as *const u8, rgba.as_mut_ptr(), rgba.len());
        }
        GdipBitmapUnlockBits(bmp, &mut data);
        swap_rb(&mut rgba);

        GdipDeleteGraphics(g);
        GdipDisposeImage(bmp as _);
        (w, h, rgba)
    }
}

// ---------- Dock 绘制 ----------

pub struct DockRender {
    /// 整幅 straight-alpha RGBA 缓冲
    pub rgba: Vec<u8>,
    /// 每个应用图标的槽位矩形（client 坐标 [x, y, size]）
    pub rects: Vec<[f32; 3]>,
    /// 每个托盘图标的槽位矩形（client 坐标 [x, y, size]）
    pub tray_rects: Vec<[f32; 3]>,
}

/// Dock 总宽度（与 draw_dock 布局严格一致）。
pub fn dock_width(n: usize, n_tray: usize, scale: f32) -> f32 {
    let pad = cfg::DOCK_PAD_X * scale;
    let slot = cfg::DOCK_SLOT * scale;
    let gap = cfg::DOCK_GAP * scale;
    let mut w = pad * 2.0 + n as f32 * slot + n.saturating_sub(1) as f32 * gap;
    if cfg::SHOW_TRAY && n_tray > 0 {
        if n > 0 {
            w += cfg::DOCK_TRAY_SEP * scale;
        }
        w += 1.0 + cfg::DOCK_TRAY_SEP * scale
            + n_tray as f32 * cfg::DOCK_TRAY_SLOT * scale
            + (n_tray as f32 - 1.0) * cfg::DOCK_TRAY_GAP * scale;
    }
    w
}

/// 绘制底部 Dock：圆角底板 + 应用图标槽位 + 分隔线 + 托盘图标槽位。
pub fn draw_dock(
    w: i32, h: i32, scale: f32,
    icons: &[&[u8]], hovered: Option<usize>,
    tray_icons: &[&[u8]], hovered_tray: Option<usize>,
) -> DockRender {
    unsafe {
        let mut bmp: *mut GpBitmap = null_mut();
        GdipCreateBitmapFromScan0(w, h, w * 4, PX_32BPP_ARGB, None, &mut bmp);
        if bmp.is_null() {
            return DockRender { rgba: vec![0; (w * h * 4) as usize], rects: vec![], tray_rects: vec![] };
        }
        let mut g: *mut GpGraphics = null_mut();
        GdipGetImageGraphicsContext(bmp as _, &mut g);
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);

        round_rect(g, 0.0, 0.0, w as f32, h as f32, cfg::DOCK_RADIUS * scale, cfg::BG_ARGB);

        let pad = cfg::DOCK_PAD_X * scale;
        let slot = cfg::DOCK_SLOT * scale;
        let gap = cfg::DOCK_GAP * scale;
        let isz = cfg::DOCK_ICON * scale;
        let mut x = pad;
        let mut rects = Vec::with_capacity(icons.len());
        for i in 0..icons.len() {
            let ry = (h as f32 - slot) / 2.0;
            if Some(i) == hovered {
                round_rect(g, x, ry, slot, slot, 12.0 * scale, cfg::HOVER_ARGB);
            }
            rects.push([x, ry, slot]);
            x += slot + gap;
        }
        if !rects.is_empty() {
            x -= gap;
        }

        // 分隔线 + 托盘图标（无图标时整块不画，避免右侧留白）
        let mut tray_rects = Vec::new();
        if cfg::SHOW_TRAY && !tray_icons.is_empty() {
            if !rects.is_empty() {
                x += cfg::DOCK_TRAY_SEP * scale;
            }
            let div_y = ((h as f32 - cfg::DIVIDER_H * scale) / 2.0) as i32;
            let div_h = (cfg::DIVIDER_H * scale) as i32;
            let div = solid_fill(cfg::DIVIDER_ARGB);
            GdipFillRectangleI(g, div as *mut GpBrush, x as i32, div_y, 1, div_h);
            GdipDeleteBrush(div as *mut GpBrush);
            x += 1.0 + cfg::DOCK_TRAY_SEP * scale;

            let tslot = cfg::DOCK_TRAY_SLOT * scale;
            for i in 0..tray_icons.len() {
                let ry = (h as f32 - tslot) / 2.0;
                if Some(i) == hovered_tray {
                    let hb = solid_fill(cfg::HOVER_ARGB);
                    GdipFillEllipseI(g, hb as *mut GpBrush, x as i32, ry as i32, tslot as i32, tslot as i32);
                    GdipDeleteBrush(hb as *mut GpBrush);
                }
                tray_rects.push([x, ry, tslot]);
                x += tslot + cfg::DOCK_TRAY_GAP * scale;
            }
        }

        let rect = GpRect { X: 0, Y: 0, Width: w, Height: h };
        let mut data = BitmapData::default();
        GdipBitmapLockBits(bmp, &rect, ImageLockModeRead.0 as u32, PX_32BPP_ARGB, &mut data);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        if !data.Scan0.is_null() {
            std::ptr::copy_nonoverlapping(data.Scan0 as *const u8, rgba.as_mut_ptr(), rgba.len());
        }
        GdipBitmapUnlockBits(bmp, &mut data);
        // GDI+ 的 32bppARGB 在内存里是 BGRA，统一换成 RGBA 再和图标合成
        swap_rb(&mut rgba);

        let isz_px = isz.max(1.0) as i32;
        for (i, buf) in icons.iter().enumerate() {
            if buf.is_empty() {
                continue;
            }
            if let Some(&[rx, ry, s]) = rects.get(i) {
                let ox = (rx + (s - isz) / 2.0).round() as i32;
                let oy = (ry + (s - isz) / 2.0).round() as i32;
                blend_over(&mut rgba, buf, ox, oy, isz_px, w);
            }
        }

        // 托盘图标（tray.rs 已按 DOCK_TRAY_ICON 缩放好，这里直接合成）
        let tisz = cfg::DOCK_TRAY_ICON * scale;
        let tisz_px = tisz.max(1.0) as i32;
        for (i, buf) in tray_icons.iter().enumerate() {
            if buf.is_empty() {
                continue;
            }
            if let Some(&[rx, ry, s]) = tray_rects.get(i) {
                let ox = (rx + (s - tisz) / 2.0).round() as i32;
                let oy = (ry + (s - tisz) / 2.0).round() as i32;
                blend_over(&mut rgba, buf, ox, oy, tisz_px, w);
            }
        }

        GdipDeleteGraphics(g);
        GdipDisposeImage(bmp as _);
        DockRender { rgba, rects, tray_rects }
    }
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

pub struct PanelRender {
    pub rgba: Vec<u8>,
    /// 命中区域 ([x, y, w, h], 动作号)
    pub hits: Vec<([f32; 4], u32)>,
}

/// 行体（复制/打开）的动作号基数；固定按钮 = PIN_BASE + 行号；标签页 = TAB_BASE + 序号
pub const PIN_BASE: u32 = 1000;
pub const TAB_BASE: u32 = 2000;

fn row_width(row: &PanelRow, scale: f32) -> f32 {
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
        inner = inner.max(row_width(r, scale));
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

/// 绘制面板：圆角底板 + 标题区（含右侧标签页）+ 行列表
pub fn draw_panel(
    w: i32, h: i32, scale: f32,
    title: &str, subtitle: &str,
    rows: &[PanelRow], empty: &str,
    tabs: &[String], tab_active: usize,
    hovered: Option<u32>,
) -> PanelRender {
    unsafe {
        let mut bmp: *mut GpBitmap = null_mut();
        GdipCreateBitmapFromScan0(w, h, w * 4, PX_32BPP_ARGB, None, &mut bmp);
        if bmp.is_null() {
            return PanelRender { rgba: vec![0; (w * h * 4) as usize], hits: vec![] };
        }
        let mut g: *mut GpGraphics = null_mut();
        GdipGetImageGraphicsContext(bmp as _, &mut g);
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
        GdipSetTextRenderingHint(g, TextRenderingHintAntiAliasGridFit);

        let (fw, fh) = (w as f32, h as f32);
        round_rect(g, 0.0, 0.0, fw, fh, cfg::PANEL_RADIUS * scale, cfg::PANEL_BG_ARGB);
        round_stroke(g, 0.5, 0.5, fw - 1.0, fh - 1.0, cfg::PANEL_RADIUS * scale, cfg::PANEL_BORDER_ARGB);

        let pad = cfg::PANEL_PAD * scale;
        let head_h = (cfg::PANEL_HEAD_H - if subtitle.is_empty() { 12.0 } else { 0.0 }) * scale;
        let t = to_u16(title);
        let (_, th) = measure_text(&t, cfg::PANEL_TITLE_SIZE, FontKind::Cjk, scale);
        if subtitle.is_empty() {
            draw_string(g, &t, cfg::TIME_ARGB, FontKind::Cjk, cfg::PANEL_TITLE_SIZE * scale,
                pad, (head_h - th) / 2.0, fw, fh);
        } else {
            draw_string(g, &t, cfg::TIME_ARGB, FontKind::Cjk, cfg::PANEL_TITLE_SIZE * scale,
                pad, head_h / 2.0 - th + 2.0 * scale, fw, fh);
            let s = to_u16(subtitle);
            let (_, sh) = measure_text(&s, cfg::PANEL_SUB_SIZE, FontKind::Cjk, scale);
            draw_string(g, &s, cfg::DATE_ARGB, FontKind::Cjk, cfg::PANEL_SUB_SIZE * scale,
                pad, head_h - sh - 4.0 * scale, fw, fh);
        }

        let mut hits: Vec<([f32; 4], u32)> = Vec::new();

        // 右上标签页（未读 / 全部）
        if !tabs.is_empty() {
            let tab_h = 22.0 * scale;
            let y = (head_h - tab_h) / 2.0;
            let mut x = fw - pad - tabs_width(tabs, scale);
            for (k, label) in tabs.iter().enumerate() {
                let tw = tab_width(label, scale);
                let rect = [x, y, tw, tab_h];
                let active = k == tab_active;
                if active {
                    round_rect(g, x, y, tw, tab_h, 7.0 * scale, cfg::HOVER_ARGB);
                } else if hovered == Some(TAB_BASE + k as u32) {
                    round_rect(g, x, y, tw, tab_h, 7.0 * scale, 0x14FF_FFFF);
                }
                let txt = to_u16(label);
                let (lw, lh) = measure_text(&txt, cfg::PANEL_SUB_SIZE, FontKind::Cjk, scale);
                let argb = if active { cfg::TIME_ARGB } else { cfg::DATE_ARGB };
                draw_string(g, &txt, argb, FontKind::Cjk, cfg::PANEL_SUB_SIZE * scale,
                    x + (tw - lw) / 2.0, y + (tab_h - lh) / 2.0, fw, fh);
                hits.push((rect, TAB_BASE + k as u32));
                x += tw + 4.0 * scale;
            }
        }

        let row_h = cfg::PANEL_ROW_H * scale;
        if rows.is_empty() {
            let e = to_u16(empty);
            let (_, eh) = measure_text(&e, cfg::PANEL_ROW_SUB, FontKind::Cjk, scale);
            draw_string(g, &e, cfg::DATE_ARGB, FontKind::Cjk, cfg::PANEL_ROW_SUB * scale,
                pad, head_h + (row_h - eh) / 2.0, fw, fh);
        }
        for (i, row) in rows.iter().enumerate() {
            let y = head_h + i as f32 * row_h;
            let id = i as u32;
            hits.push(([pad * 0.5, y, fw - pad, row_h], id));
            if hovered == Some(id) {
                round_rect(g, pad * 0.5, y + 1.0, fw - pad, row_h - 2.0, 8.0 * scale, cfg::HOVER_ARGB);
            }

            let mut x = pad;
            if !row.icon.is_empty() {
                let icon = to_u16(&row.icon);
                let (iw, ih) = measure_text(&icon, cfg::WIDGET_ICON_SIZE, FontKind::Symbol, scale);
                draw_string(g, &icon, row.icon_argb, FontKind::Symbol, cfg::WIDGET_ICON_SIZE * scale,
                    x, y + (row_h - ih) / 2.0, fw, fh);
                x += iw + cfg::PANEL_ICON_GAP * scale;
            }

            // 右侧：固定按钮占位 → 数值右对齐
            let pin_w = if row.pin.is_some() { cfg::PIN_W * scale } else { 0.0 };
            let vw = if row.value.is_empty() {
                0.0
            } else {
                measure_text(&to_u16(&row.value), cfg::PANEL_VALUE_SIZE, FontKind::Cjk, scale).0
            };
            let room = (fw - pad - pin_w - vw - cfg::PANEL_VALUE_GAP * scale - x).max(24.0);
            if row.sub.is_empty() {
                let tt = ellipsize(&row.title, room, cfg::PANEL_ROW_TITLE, FontKind::Cjk, scale);
                let t = to_u16(&tt);
                let (_, th) = measure_text(&t, cfg::PANEL_ROW_TITLE, FontKind::Cjk, scale);
                draw_string(g, &t, cfg::TEXT_ARGB, FontKind::Cjk, cfg::PANEL_ROW_TITLE * scale,
                    x, y + (row_h - th) / 2.0, fw, fh);
            } else {
                let tt = ellipsize(&row.title, room, cfg::PANEL_ROW_TITLE, FontKind::Cjk, scale);
                let t = to_u16(&tt);
                draw_string(g, &t, cfg::TEXT_ARGB, FontKind::Cjk, cfg::PANEL_ROW_TITLE * scale,
                    x, y + 8.0 * scale, fw, fh);
                let ss = ellipsize(&row.sub, room, cfg::PANEL_ROW_SUB, FontKind::Cjk, scale);
                let s = to_u16(&ss);
                draw_string(g, &s, cfg::DATE_ARGB, FontKind::Cjk, cfg::PANEL_ROW_SUB * scale,
                    x, y + 26.0 * scale, fw, fh);
            }
            if vw > 0.0 {
                let v = to_u16(&row.value);
                let (rw, rh) = measure_text(&v, cfg::PANEL_VALUE_SIZE, FontKind::Cjk, scale);
                draw_string(g, &v, row.value_argb, FontKind::Cjk, cfg::PANEL_VALUE_SIZE * scale,
                    fw - pad - pin_w - rw, y + (row_h - rh) / 2.0, fw, fh);
            }
            if let Some(pinned) = row.pin {
                let (star, argb) = if pinned { ("★", cfg::PINNED_ARGB) } else { ("☆", cfg::TEXT_DIM_ARGB) };
                let st = to_u16(star);
                let (sw, sh) = measure_text(&st, 13.0, FontKind::Symbol, scale);
                let sx = fw - pad - pin_w + (pin_w - sw) / 2.0;
                draw_string(g, &st, argb, FontKind::Symbol, 13.0 * scale,
                    sx, y + (row_h - sh) / 2.0, fw, fh);
                hits.push(([fw - pad - pin_w, y, pin_w, row_h], PIN_BASE + id));
            }
        }

        let rect = GpRect { X: 0, Y: 0, Width: w, Height: h };
        let mut data = BitmapData::default();
        GdipBitmapLockBits(bmp, &rect, ImageLockModeRead.0 as u32, PX_32BPP_ARGB, &mut data);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        if !data.Scan0.is_null() {
            std::ptr::copy_nonoverlapping(data.Scan0 as *const u8, rgba.as_mut_ptr(), rgba.len());
        }
        GdipBitmapUnlockBits(bmp, &mut data);
        // GDI+ 的 32bppARGB 在内存里是 BGRA，统一换成 RGBA 再和图标合成
        swap_rb(&mut rgba);

        GdipDeleteGraphics(g);
        GdipDisposeImage(bmp as _);
        PanelRender { rgba, hits }
    }
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
const LEFT_SUB_DY: f32 = 58.0;
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

/// 展开的天气卡片：左侧实况，右侧四天预报，右上刷新按钮。
/// 所有字号与纵向位置都乘字号倍率 k（settings::weather_text_scale）。
/// 返回的 hits 只有 [0] = 刷新按钮的命中矩形。
pub fn draw_weather_panel(w: i32, h: i32, scale: f32, v: &WeatherView, hover_refresh: bool) -> PanelRender {
    unsafe {
        let mut bmp: *mut GpBitmap = null_mut();
        GdipCreateBitmapFromScan0(w, h, w * 4, PX_32BPP_ARGB, None, &mut bmp);
        if bmp.is_null() {
            return PanelRender { rgba: vec![0; (w * h * 4) as usize], hits: vec![] };
        }
        let mut g: *mut GpGraphics = null_mut();
        GdipGetImageGraphicsContext(bmp as _, &mut g);
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
        GdipSetTextRenderingHint(g, TextRenderingHintAntiAliasGridFit);

        let (fw, fh) = (w as f32, h as f32);
        round_rect(g, 0.0, 0.0, fw, fh, cfg::PANEL_RADIUS * scale, cfg::PANEL_BG_ARGB);
        round_stroke(g, 0.5, 0.5, fw - 1.0, fh - 1.0, cfg::PANEL_RADIUS * scale, cfg::PANEL_BORDER_ARGB);

        let pad = 16.0 * scale;
        let k = crate::settings::weather_text_scale();
        let s = |v: f32| v * scale;
        // 逻辑字号 → 物理像素（含字号倍率）
        let fs = |v: f32| v * k * scale;
        // 左栏宽度：图标 + 文字列，随字号略放宽
        let left_w = s(cfg::WEATHER_LEFT_W) + s(56.0 * (k - 1.0));

        // 标题：当地(白) + 天气(强调色)
        let t1 = to_u16("当地");
        let t2 = to_u16("天气");
        let (w1, _) = measure_text(&t1, 15.0 * k, FontKind::Cjk, scale);
        draw_string(g, &t1, cfg::TIME_ARGB, FontKind::Cjk, fs(15.0), pad, s(9.0 * k), fw, fh);
        draw_string(g, &t2, cfg::ACCENT_ARGB, FontKind::Cjk, fs(15.0), pad + w1, s(9.0 * k), fw, fh);

        // 右上：刷新（先定位，日期时间再来凑）
        let refresh_w = s(28.0 * k);
        let refresh_rect = [fw - pad - refresh_w, s(7.0 * k), refresh_w, s(22.0 * k)];
        if hover_refresh {
            round_rect(g, refresh_rect[0] - s(6.0), refresh_rect[1], refresh_rect[2] + s(12.0),
                refresh_rect[3], s(6.0), cfg::HOVER_ARGB);
        }
        let rl = to_u16("刷新");
        let (rw, rh) = measure_text(&rl, cfg::WEATHER_DETAIL_SIZE * k, FontKind::Cjk, scale);
        draw_string(g, &rl, cfg::DATE_ARGB, FontKind::Cjk, fs(cfg::WEATHER_DETAIL_SIZE),
            refresh_rect[0] + (refresh_w - rw) / 2.0, refresh_rect[1] + (refresh_rect[3] - rh) / 2.0, fw, fh);

        // 时间 + 日期（右对齐到刷新左侧）
        let time = to_u16(&v.time);
        let (tw, th) = measure_text(&time, 14.0 * k, FontKind::Latin, scale);
        let tx = refresh_rect[0] - s(14.0 * k) - tw;
        draw_string(g, &time, cfg::TIME_ARGB, FontKind::Latin, fs(14.0), tx, s(9.0 * k), fw, fh);
        let date = to_u16(&v.date);
        let (dw, dh) = measure_text(&date, cfg::WEATHER_DETAIL_SIZE * k, FontKind::Cjk, scale);
        draw_string(g, &date, cfg::DATE_ARGB, FontKind::Cjk, fs(cfg::WEATHER_DETAIL_SIZE),
            tx - s(8.0 * k) - dw, s(9.0 * k) + (th - dh) / 2.0, fw, fh);

        // ---- 左：实况 ----
        // 结构：① 图标 + 大号温度同一行；② 天气 · 城市；③ 详情（最多两行）+ 数据来源。
        // 整块在可用高度里居中；纵向节奏与定高用同一组常量（LEFT_*）。
        // 左栏顶基准比右栏略高：Symbol / Latin 字体的行框上边有留白，
        // 这样首行墨迹（图标、温度）才和右栏的"今天"齐平。
        let left_y = s(34.0 * k);
        let top = left_y;

        let temp = to_u16(&v.temp);
        let cond = to_u16(&v.condition);
        let city = to_u16(&v.city);
        let rep = to_u16(&v.report);
        let icon = to_u16(&v.icon);
        let (cond_w, ch) = measure_text(&cond, cfg::WEATHER_COND_SIZE * k, FontKind::Cjk, scale);
        let (_cyw, cyh) = measure_text(&city, cfg::WEATHER_CITY_SIZE * k, FontKind::Cjk, scale);

        // ① 图标与温度同一行
        let icon_box = s(60.0 * k);
        let (iw, _ih) = measure_text(&icon, cfg::WEATHER_ICON_SIZE * k, FontKind::Symbol, scale);
        draw_string(g, &icon, v.icon_argb, FontKind::Symbol, fs(cfg::WEATHER_ICON_SIZE),
            pad + (icon_box - iw) / 2.0, top, fw, fh);
        let tx = pad + icon_box + s(10.0 * k);
        draw_string(g, &temp, cfg::TIME_ARGB, FontKind::Latin, fs(cfg::WEATHER_TEMP_SIZE),
            tx, top + s(2.0 * k), fw, fh);

        // ② 天气 · 城市（城市小一号、压暗，保持层级）
        let sub_y = top + s(LEFT_SUB_DY * k);
        draw_string(g, &cond, cfg::TIME_ARGB, FontKind::Cjk, fs(cfg::WEATHER_COND_SIZE), pad, sub_y, fw, fh);
        draw_string(g, &city, cfg::DATE_ARGB, FontKind::Cjk, fs(cfg::WEATHER_CITY_SIZE),
            pad + cond_w + s(8.0 * k), sub_y + (ch - cyh), fw, fh);

        // ③ 详情 + 来源
        let mut y = top + s(LEFT_DET_DY * k);
        for line in v.details.iter().take(2) {
            let txt = ellipsize(line, left_w, cfg::WEATHER_DETAIL_SIZE * k, FontKind::Cjk, scale);
            let t = to_u16(&txt);
            draw_string(g, &t, cfg::DATE_ARGB, FontKind::Cjk, fs(cfg::WEATHER_DETAIL_SIZE), pad, y, fw, fh);
            y += s(LEFT_DET_STEP * k);
        }
        draw_string(g, &rep, cfg::TEXT_DIM_ARGB, FontKind::Cjk, fs(10.0), pad, y + s(LEFT_REP_GAP * k), fw, fh);

        // 竖分隔线
        let divx = pad + left_w + s(6.0);
        let div = solid_fill(cfg::DIVIDER_ARGB);
        let div_top = left_y;
        GdipFillRectangleI(g, div as *mut GpBrush, divx as i32, div_top as i32, 1, (s(140.0 * k)) as i32);
        GdipDeleteBrush(div as *mut GpBrush);

        // ---- 右：四天 ----
        let days_y = s(44.0 * k);
        let cols = v.days.len().min(4) as f32;
        if cols > 0.0 {
            let start = divx + s(12.0 * k);
            let cw = (fw - start - pad) / cols;
            for (i, d) in v.days.iter().take(4).enumerate() {
                let center = start + i as f32 * cw + cw / 2.0;

                let label = to_u16(&d.label);
                let (lw, _) = measure_text(&label, cfg::WEATHER_DAY_LABEL * k, FontKind::Cjk, scale);
                let argb = if i == 0 { cfg::TIME_ARGB } else { cfg::DATE_ARGB };
                draw_string(g, &label, argb, FontKind::Cjk, fs(cfg::WEATHER_DAY_LABEL),
                    center - lw / 2.0, days_y + s(1.0 * k), fw, fh);

                let ic = to_u16(&d.icon);
                let (icw, _) = measure_text(&ic, cfg::WEATHER_DAY_ICON * k, FontKind::Symbol, scale);
                draw_string(g, &ic, d.icon_argb, FontKind::Symbol, fs(cfg::WEATHER_DAY_ICON),
                    center - icw / 2.0, days_y + s(21.0 * k), fw, fh);

                let high = to_u16(&d.high);
                let (hw, _) = measure_text(&high, cfg::WEATHER_DAY_HIGH * k, FontKind::Latin, scale);
                draw_string(g, &high, cfg::TIME_ARGB, FontKind::Latin, fs(cfg::WEATHER_DAY_HIGH),
                    center - hw / 2.0, days_y + s(46.0 * k), fw, fh);

                let low = to_u16(&d.low);
                let (lw2, _) = measure_text(&low, cfg::WEATHER_DAY_LOW * k, FontKind::Cjk, scale);
                draw_string(g, &low, cfg::DATE_ARGB, FontKind::Cjk, fs(cfg::WEATHER_DAY_LOW),
                    center - lw2 / 2.0, days_y + s(66.0 * k), fw, fh);

                if !d.text.is_empty() {
                    let txt = ellipsize(&d.text, cw - s(2.0), cfg::WEATHER_DAY_TEXT * k, FontKind::Cjk, scale);
                    let t = to_u16(&txt);
                    let (tw2, _) = measure_text(&t, cfg::WEATHER_DAY_TEXT * k, FontKind::Cjk, scale);
                    draw_string(g, &t, cfg::TEXT_ARGB, FontKind::Cjk, fs(cfg::WEATHER_DAY_TEXT),
                        center - tw2 / 2.0, days_y + s(86.0 * k), fw, fh);
                }
                if !d.note.is_empty() {
                    let txt = ellipsize(&d.note, cw - s(2.0), cfg::WEATHER_DAY_WIND * k, FontKind::Cjk, scale);
                    let t = to_u16(&txt);
                    let (nw, _) = measure_text(&t, cfg::WEATHER_DAY_WIND * k, FontKind::Cjk, scale);
                    draw_string(g, &t, d.note_argb, FontKind::Cjk, fs(cfg::WEATHER_DAY_WIND),
                        center - nw / 2.0, days_y + s(103.0 * k), fw, fh);
                }
            }
        }

        let rect = GpRect { X: 0, Y: 0, Width: w, Height: h };
        let mut data = BitmapData::default();
        GdipBitmapLockBits(bmp, &rect, ImageLockModeRead.0 as u32, PX_32BPP_ARGB, &mut data);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        if !data.Scan0.is_null() {
            std::ptr::copy_nonoverlapping(data.Scan0 as *const u8, rgba.as_mut_ptr(), rgba.len());
        }
        GdipBitmapUnlockBits(bmp, &mut data);
        // GDI+ 的 32bppARGB 在内存里是 BGRA，统一换成 RGBA 再和图标合成
        swap_rb(&mut rgba);

        GdipDeleteGraphics(g);
        GdipDisposeImage(bmp as _);
        PanelRender { rgba, hits: vec![(refresh_rect, 0)] }
    }
}

/// 文本按像素宽度截断并加省略号
pub fn ellipsize(text: &str, max_w: f32, size: f32, kind: FontKind, scale: f32) -> String {
    if measure_text(&to_u16(text), size, kind, scale).0 <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut trial = out.clone();
        trial.push(ch);
        trial.push('…');
        if measure_text(&to_u16(&trial), size, kind, scale).0 > max_w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
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
            let slice = std::slice::from_raw_parts(bits as *const u32, (px * px) as usize);
            let mut rgba = vec![0u8; (px * px * 4) as usize];
            for (i, v) in slice.iter().enumerate() {
                let a = (v >> 24) & 0xFF;
                // DIB 内存序是 BGRA，读成 u32 后：A<<24 | R<<16 | G<<8 | B
                let r_ = (v >> 16) & 0xFF;
                let g_ = (v >> 8) & 0xFF;
                let b = v & 0xFF;
                let (j0, j1, j2, j3) = (i * 4, i * 4 + 1, i * 4 + 2, i * 4 + 3);
                if a == 0 {
                    rgba[j0] = 0;
                    rgba[j1] = 0;
                    rgba[j2] = 0;
                    rgba[j3] = 0;
                } else if a == 255 {
                    rgba[j0] = r_ as u8;
                    rgba[j1] = g_ as u8;
                    rgba[j2] = b as u8;
                    rgba[j3] = 255;
                } else {
                    // premultiplied BGRA → straight RGBA
                    rgba[j0] = ((r_ * 255 + a / 2) / a) as u8;
                    rgba[j1] = ((g_ * 255 + a / 2) / a) as u8;
                    rgba[j2] = ((b * 255 + a / 2) / a) as u8;
                    rgba[j3] = a as u8;
                }
            }
            out = Some(rgba);
        }
        let _ = DeleteObject(hbmp);
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), hdc);
        out
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

/// straight-alpha RGBA → premultiplied BGRA（UpdateLayeredWindow 要求）。
pub fn to_premultiplied_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for (i, chunk) in rgba.chunks_exact(4).enumerate() {
        let a = chunk[3] as u32;
        let (j0, j1, j2, j3) = (i * 4, i * 4 + 1, i * 4 + 2, i * 4 + 3);
        out[j0] = ((chunk[2] as u32 * a + 127) / 255) as u8; // B
        out[j1] = ((chunk[1] as u32 * a + 127) / 255) as u8; // G
        out[j2] = ((chunk[0] as u32 * a + 127) / 255) as u8; // R
        out[j3] = chunk[3];
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

unsafe fn create_font(kind: FontKind, px: f32, font: &mut *mut GpFont) -> bool {
    let name: PCWSTR = match kind {
        FontKind::Cjk => w!("Microsoft YaHei UI"),
        FontKind::Symbol => w!("Segoe UI Symbol"),
        FontKind::Latin => w!("Segoe UI Variable Display"),
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

unsafe fn solid_fill(argb: u32) -> *mut GpSolidFill {
    let mut b: *mut GpSolidFill = null_mut();
    GdipCreateSolidFill(argb, &mut b);
    b
}

/// 圆角矩形填充（半径自动收敛到半宽/半高）
unsafe fn round_rect(g: *mut GpGraphics, x: f32, y: f32, w: f32, h: f32, r: f32, argb: u32) {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut path: *mut GpPath = null_mut();
    GdipCreatePath(FillModeWinding, &mut path);
    GdipAddPathArc(path, x, y, 2.0 * r, 2.0 * r, 180.0, 90.0);
    GdipAddPathArc(path, x + w - 2.0 * r, y, 2.0 * r, 2.0 * r, 270.0, 90.0);
    GdipAddPathArc(path, x + w - 2.0 * r, y + h - 2.0 * r, 2.0 * r, 2.0 * r, 0.0, 90.0);
    GdipAddPathArc(path, x, y + h - 2.0 * r, 2.0 * r, 2.0 * r, 90.0, 90.0);
    GdipClosePathFigure(path);
    let b = solid_fill(argb);
    GdipFillPath(g, b as *mut GpBrush, path);
    GdipDeleteBrush(b as *mut GpBrush);
    GdipDeletePath(path);
}

/// 圆角矩形描边（1px 边框）
unsafe fn round_stroke(g: *mut GpGraphics, x: f32, y: f32, w: f32, h: f32, r: f32, argb: u32) {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut path: *mut GpPath = null_mut();
    GdipCreatePath(FillModeWinding, &mut path);
    GdipAddPathArc(path, x, y, 2.0 * r, 2.0 * r, 180.0, 90.0);
    GdipAddPathArc(path, x + w - 2.0 * r, y, 2.0 * r, 2.0 * r, 270.0, 90.0);
    GdipAddPathArc(path, x + w - 2.0 * r, y + h - 2.0 * r, 2.0 * r, 2.0 * r, 0.0, 90.0);
    GdipAddPathArc(path, x, y + h - 2.0 * r, 2.0 * r, 2.0 * r, 90.0, 90.0);
    GdipClosePathFigure(path);
    let mut pen: *mut windows::Win32::Graphics::GdiPlus::GpPen = null_mut();
    GdipCreatePen1(argb, 1.0, UnitPixel, &mut pen);
    if !pen.is_null() {
        GdipDrawPath(g, pen, path);
        GdipDeletePen(pen);
    }
    GdipDeletePath(path);
}

unsafe fn draw_string(
    g: *mut GpGraphics, text: &[u16], argb: u32, kind: FontKind, px: f32, x: f32, y: f32, w: f32, h: f32,
) {
    let mut font = null_mut();
    if !create_font(kind, px, &mut font) { return; }
    let mut fmt = null_mut();
    GdipStringFormatGetGenericDefault(&mut fmt);
    let brush = solid_fill(argb);
    let layout = RectF { X: x, Y: y, Width: w as f32, Height: h as f32 };
    GdipDrawString(g, PCWSTR(text.as_ptr()), text.len() as i32, font, &layout, fmt, brush as *const GpBrush);
    GdipDeleteBrush(brush as *mut GpBrush);
    GdipDeleteStringFormat(fmt);
    GdipDeleteFont(font);
}

/// straight-alpha over straight-alpha 软件合成
fn blend_over(dst: &mut [u8], src: &[u8], x: i32, y: i32, s: i32, buf_w: i32) {
    if s <= 0 { return; }
    let buf_h = dst.len() as i32 / 4 / buf_w.max(1);
    for iy in 0..s {
        let dy = y + iy;
        if dy < 0 || dy >= buf_h { continue; }
        for ix in 0..s {
            let dx = x + ix;
            if dx < 0 || dx >= buf_w { continue; }
            let di = ((dy * buf_w + dx) * 4) as usize;
            let si = ((iy * s + ix) * 4) as usize;
            if di + 3 >= dst.len() || si + 3 >= src.len() { continue; }
            let sa = src[si + 3] as f32 / 255.0;
            if sa <= 0.0 { continue; }
            let da = dst[di + 3] as f32 / 255.0;
            let oa = sa + da * (1.0 - sa);
            if oa <= 0.0 {
                dst[di] = 0;
                dst[di + 1] = 0;
                dst[di + 2] = 0;
                dst[di + 3] = 0;
                continue;
            }
            for c in 0..3 {
                let sc = src[si + c] as f32;
                let dc = dst[di + c] as f32;
                dst[di + c] = ((sc * sa + dc * da * (1.0 - sa)) / oa).round() as u8;
            }
            dst[di + 3] = (oa * 255.0).round() as u8;
        }
    }
}
