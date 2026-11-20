//! 音乐条窗口：贴在顶部胶囊左/右/下方的一条独立分层小窗，
//! 内容 = 左侧旋转的专辑封面（中心是播放/暂停按钮）+ 右侧当前这句歌词。
//! 原胶囊里的音乐 widget 已并进来：没有歌曲播放时整条隐藏，不留占位。
//!
//! 整窗默认鼠标穿透，只有封面那个圆接收鼠标（左键播放/暂停、右键下一首）；
//! 点击不会自己处理业务，而是 PostMessage 给胶囊窗口，由 `App` 分发。
//! 显隐、内容、角度与几何全部由胶囊窗口驱动（`App::tick_bar`）。

use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, LoadCursorW, PostMessageW, RegisterClassW, SetWindowPos,
    ShowWindow, HMENU, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW, SWP_NOACTIVATE, SW_HIDE,
    SW_SHOWNOACTIVATE, WM_APP, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCHITTEST, WM_RBUTTONDOWN,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WNDCLASSW,
};

use crate::config as cfg;
use crate::render;
use crate::settings::LyricPos;

/// 点了封面中心的按钮（播放/暂停）
pub const WM_TOGGLE: u32 = WM_APP + 0x21;
/// 右键封面 = 下一首
pub const WM_NEXT: u32 = WM_APP + 0x22;

thread_local! {
    static WIN: Cell<isize> = const { Cell::new(0) };
    /// egui 软件后端（懒初始化）
    static VIEW: RefCell<Option<crate::egui_soft::SoftBackend>> = const { RefCell::new(None) };
    /// 胶囊窗口（点击事件回传给它）
    static OWNER: Cell<isize> = const { Cell::new(0) };
    /// 上一次上屏的内容与几何：一样就不再重画（音乐条每 200ms / 每帧被问一次）
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
    /// 封面圆形命中区（client 坐标：圆心 x, y, 半径）
    static HIT: Cell<(f32, f32, f32)> = const { Cell::new((0.0, 0.0, 0.0)) };
    /// 鼠标是否压在封面上（决定按钮底色）
    static HOVER: Cell<bool> = const { Cell::new(false) };
    static TRACKED: Cell<bool> = const { Cell::new(false) };
}

/// 已经画在屏幕上的那条音乐条
#[derive(PartialEq)]
struct Shown {
    text: String,
    angle: i32,
    playing: bool,
    hovered: bool,
    cover_id: u64,
    rect: (i32, i32, i32, i32),
}

/// 一帧要显示的内容（由 `App::tick_bar` 组装）
pub struct View<'a> {
    /// 当前这句歌词；没有歌词时是"歌名 · 歌手"
    pub text: &'a str,
    /// 解码好的正方形封面（px*px*4 straight RGBA）；None = 没有封面
    pub cover: Option<&'a [u8]>,
    /// 封面版本号（换曲即变），只用于缓存键
    pub cover_id: u64,
    /// 封面旋转角度（度）
    pub angle: f32,
    pub playing: bool,
    pub scale: f32,
    pub font: f32,
    /// 胶囊的屏幕矩形 (x, y, w, h)
    pub pill: (i32, i32, i32, i32),
    pub pos: LyricPos,
}

pub fn init(instance: HINSTANCE, owner: HWND) {
    OWNER.with(|c| c.set(owner.0 as isize));
    unsafe {
        let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: arrow,
            lpszClassName: w!("ORBIT_MUSICBAR"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        if let Ok(h) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("ORBIT_MUSICBAR"),
            PCWSTR::null(),
            WS_POPUP,
            0, 0, 10, 10,
            HWND::default(),
            HMENU::default(),
            instance,
            None,
        ) {
            WIN.with(|c| c.set(h.0 as isize));
        }
    }
}

fn hwnd() -> HWND {
    HWND(WIN.with(|c| c.get()) as *mut _)
}

/// 鼠标是否压在封面上
pub fn hovered() -> bool {
    HOVER.with(|c| c.get())
}

/// 显示一帧。`pos` 决定贴在胶囊哪一侧，宽度不够时按省略号截断。
pub fn show(v: &View) {
    let h = hwnd();
    if h.is_invalid() || v.text.is_empty() {
        hide();
        return;
    }
    let m = crate::window::primary_monitor().rcMonitor;
    // 音乐条与胶囊等高：胶囊大小设置同样作用在这里（DPI × 胶囊缩放）
    let scale = (v.scale * crate::settings::pill_scale()).max(0.5);
    let (px, py, pw, ph) = v.pill;
    let gap = (cfg::LYRIC_GAP as f32 * scale) as i32;

    // 该侧可用宽度：左右贴边时给胶囊让出位置，下方按整屏居中（物理像素）
    let avail = match v.pos {
        LyricPos::Left => px - gap - m.left - 6,
        LyricPos::Right => m.right - (px + pw + gap) - 6,
        LyricPos::Bottom => m.right - m.left - 12,
    } as f32;
    let max_w_px = (cfg::LYRIC_MAX_W * scale).min(avail);

    // 字体度量与布局都在逻辑点空间做（egui 内部按 pixels_per_point 缩放）
    let fixed = crate::musicbarview::fixed_w();
    let max_w_l = max_w_px / scale;
    if max_w_l < fixed + 40.0 {
        hide();
        return;
    }
    VIEW.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(crate::egui_soft::SoftBackend::new());
        }
        let soft = slot.as_mut().unwrap();
        soft.set_scale(scale);
        let text = crate::musicbarview::ellipsize(soft, v.text, max_w_l - fixed, v.font);
        let w_l = fixed + crate::musicbarview::text_w(soft, &text, v.font);
        // w_l 是逻辑点，窗口尺寸要物理像素（乘以 DPI × 胶囊缩放）
        let w = (w_l * scale).ceil() as i32;
        let hh = (cfg::PILL_H * scale).ceil() as i32;
        let (x, y) = match v.pos {
            LyricPos::Right => (px + pw + gap, py),
            LyricPos::Left => (px - gap - w, py),
            LyricPos::Bottom => (px + (pw - w) / 2, py + ph + gap),
        };
        let x = x.clamp(m.left + 6, (m.right - w - 6).max(m.left + 6));

        let angle = ((v.angle % 360.0) + 360.0) % 360.0;
        let shown = Shown {
            text,
            angle: angle.round() as i32,
            playing: v.playing,
            hovered: hovered(),
            cover_id: v.cover_id,
            rect: (x, y, w, hh),
        };

        // 命中区 = 封面的圆（物理像素；与 musicbarview::cover_box 同几何）
        let (bx, by, d) = crate::musicbarview::cover_box(cfg::PILL_H);
        let bx = bx * scale;
        let by = by * scale;
        let d = d * scale;
        HIT.with(|c| c.set((bx + d / 2.0, by + d / 2.0, d / 2.0)));

        unsafe {
            if !SHOWN.with(|s| s.borrow().as_ref() == Some(&shown)) {
                let rgba = crate::musicbarview::paint(
                    soft, w, hh, scale, &shown.text, v.font, v.cover, angle, v.playing, shown.hovered,
                );
                // egui 软件后端输出已是预乘 RGBA，直接换 BGRA 上屏
                let bgra = render::premultiplied_rgba_to_bgra(&rgba);
                render::present(h, &bgra, w, hh);
                SHOWN.with(|s| *s.borrow_mut() = Some(shown));
            }
            let _ = SetWindowPos(h, HWND_TOPMOST, x, y, w, hh, SWP_NOACTIVATE);
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
        }
    });
}

pub fn hide() {
    let h = hwnd();
    if h.is_invalid() {
        return;
    }
    unsafe {
        let _ = ShowWindow(h, SW_HIDE);
    }
    SHOWN.with(|s| *s.borrow_mut() = None);
    HIT.with(|c| c.set((0.0, 0.0, 0.0)));
    HOVER.with(|c| c.set(false));
    // 窗口藏起来后 TrackMouseEvent 的注册就失效了，下次显示要重新注册
    TRACKED.with(|c| c.set(false));
}

/// 屏幕坐标是否落在封面圆内
fn hit(scr_x: i32, scr_y: i32) -> bool {
    let h = hwnd();
    if h.is_invalid() {
        return false;
    }
    let (cx, cy, r) = HIT.with(|c| c.get());
    if r <= 0.0 {
        return false;
    }
    let mut pt = POINT { x: scr_x, y: scr_y };
    unsafe {
        let _ = ScreenToClient(h, &mut pt);
    }
    let (dx, dy) = (pt.x as f32 - cx, pt.y as f32 - cy);
    dx * dx + dy * dy <= r * r
}

fn post(msg: u32) {
    let owner = HWND(OWNER.with(|c| c.get()) as *mut _);
    if !owner.is_invalid() {
        unsafe {
            let _ = PostMessageW(owner, msg, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // 只有封面那块圆接鼠标，其余部分一律穿透
            WM_NCHITTEST => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                if hit(x, y) {
                    LRESULT(HTCLIENT as isize)
                } else {
                    LRESULT(HTTRANSPARENT as isize)
                }
            }
            WM_MOUSEMOVE => {
                // 悬停态变了不用自己重画：胶囊的定时器会在 50ms 内带着新状态重画一次
                HOVER.with(|c| c.set(true));
                if !TRACKED.with(|c| c.replace(true)) {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: h,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                TRACKED.with(|c| c.set(false));
                HOVER.with(|c| c.set(false));
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                post(WM_TOGGLE);
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                post(WM_NEXT);
                LRESULT(0)
            }
            _ => DefWindowProcW(h, msg, w, l),
        }
    }
}
