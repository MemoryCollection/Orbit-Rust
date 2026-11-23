//! 微信托盘悬停预览浮窗：鼠标停在 Dock 的微信图标上时，在图标上方弹一张
//! 未读会话卡片（会话名 + 未读数 + 摘要），点哪条就直达那个会话。
//!
//! 为什么自己做而不用微信自带的 `mmui::UnreadMessageHoverWindow`：
//! 微信那个 290×152 只放得下一行，UIA 里的会话行 `name` 还是**空的**（读不到
//! 是谁发的），而且移开鼠标就被销毁。自己画可以一次列出所有未读、按未读数排序。
//!
//! 与 banner 的分工：banner 是「来新消息了」的一次性通知（会响、会堆叠、会超时）；
//! 这里是「我悬停想看看有谁找我」的按需列表，不响、不堆叠、鼠标离开即收。
//!
//! 实现照 banner 的骨架：独立分层窗口（NOACTIVATE + 逐像素透明 + 圆角命中）、
//! egui 软件后端出像素、`render::present` 上屏。窗口线程收 PostMessage 载荷。

use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetWindowLongPtrW, GetWindowRect, PostMessageW,
    RegisterClassW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW,
    GWLP_USERDATA, HMENU, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW, LoadCursorW, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, WM_CREATE, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCHITTEST,
    WM_TIMER, WM_APP, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::config as cfg;
use crate::egui_soft::{argb, glass_board, SoftBackend};
use crate::render;
use crate::wechatmsg::WxUnread;

/// 卡片宽度（逻辑点）
const CARD_W: f32 = 330.0;
/// 单行高度（会话名 + 摘要两行）
const ROW_H: f32 = 58.0;
/// 卡片内边距
const PAD: f32 = 10.0;
/// 行间距
const ROW_GAP: f32 = 4.0;
/// 最多列几条（再多浮窗就太长了）
const MAX_ROWS: usize = 5;
/// 鼠标离开图标后给多少毫秒把指针移到浮窗上（中间有 Dock 高度 + 间隙）
const HIDE_GRACE: Duration = Duration::from_millis(360);
/// 图标中心到浮窗底边的距离：图标半高 + 一点空隙
const LIFT: i32 = 42;

const TIMER_TICK: usize = 1;
/// 显示/更新浮窗：lParam = Box<(Vec<WxUnread>, (i32, i32))>
pub const WM_WXHOVER_SHOW: u32 = WM_APP + 0x61;
/// 请求收起（走宽限期）：让鼠标有机会从图标移到卡片上
pub const WM_WXHOVER_HIDE: u32 = WM_APP + 0x62;

static HOVER_HWND: AtomicIsize = AtomicIsize::new(0);

struct Hover {
    view: SoftBackend,
    rows: Vec<WxUnread>,
    anchor: (i32, i32),
    size: (i32, i32),
    /// Some(t) = t 之后收起；None = 保持（鼠标停在卡片上）
    hide_at: Option<Instant>,
    mouse_tracked: bool,
    /// 当前鼠标悬停在哪一行（绘制高亮用）
    hot: Option<usize>,
}

fn hwnd() -> HWND {
    HWND(HOVER_HWND.load(Ordering::Relaxed) as *mut _)
}

pub fn init(instance: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("ORBIT_WXHOVER"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let p = Box::into_raw(Box::new(Hover {
            view: SoftBackend::new(),
            rows: Vec::new(),
            anchor: (0, 0),
            size: (0, 0),
            hide_at: None,
            mouse_tracked: false,
            hot: None,
        }));
        match CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("ORBIT_WXHOVER"),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            HWND::default(),
            HMENU::default(),
            instance,
            Some(p as *const core::ffi::c_void),
        ) {
            Ok(h) => {
                HOVER_HWND.store(h.0 as isize, Ordering::Relaxed);
                let _ = SetTimer(h, TIMER_TICK, 100, None);
            }
            Err(_) => drop(Box::from_raw(p)),
        }
    }
}

/// 任意线程：在 `anchor`（托盘图标中心，屏幕坐标）上方显示未读列表。
/// 列表为空时直接收起（没东西可看，交给微信自己的 tooltip）。
pub fn show(rows: Vec<WxUnread>, anchor: (i32, i32)) {
    let h = hwnd();
    if h.is_invalid() {
        return;
    }
    if rows.is_empty() {
        request_hide();
        return;
    }
    let payload = Box::into_raw(Box::new((rows, anchor)));
    unsafe {
        if PostMessageW(h, WM_WXHOVER_SHOW, WPARAM(0), LPARAM(payload as isize)).is_err() {
            drop(Box::from_raw(payload));
        }
    }
}

/// 鼠标离开图标：给一个宽限期再收（让指针能移到卡片上）
pub fn request_hide() {
    let h = hwnd();
    if h.is_invalid() {
        return;
    }
    unsafe {
        let _ = PostMessageW(h, WM_WXHOVER_HIDE, WPARAM(0), LPARAM(0));
    }
}

/// 立即收起（切走微信图标 / 点到卡片 / 设置里关掉开关时用）
pub fn hide_now() {
    let h = hwnd();
    if h.is_invalid() {
        return;
    }
    unsafe {
        let _ = ShowWindow(h, SW_HIDE);
    }
}

/// 浮窗是否正在显示（Dock 据此决定「悬停是不是已经交给我们了」）
pub fn visible() -> bool {
    let h = hwnd();
    if h.is_invalid() {
        return false;
    }
    unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(h).as_bool() }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CREATE {
            let cs = *(l.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(h, GWLP_USERDATA, cs.lpCreateParams as isize);
            return LRESULT(0);
        }
        let ptr = GetWindowLongPtrW(h, GWLP_USERDATA) as *mut Hover;
        if ptr.is_null() {
            return DefWindowProcW(h, msg, w, l);
        }
        let c = &mut *ptr;
        match msg {
            WM_WXHOVER_SHOW => {
                if l.0 != 0 {
                    let (rows, anchor) = *Box::from_raw(l.0 as *mut (Vec<WxUnread>, (i32, i32)));
                    c.accept(h, rows, anchor);
                }
                LRESULT(0)
            }
            WM_WXHOVER_HIDE => {
                // 已经在显示才需要宽限；没显示就无所谓
                if !c.rows.is_empty() && c.hide_at.is_none() {
                    c.hide_at = Some(Instant::now() + HIDE_GRACE);
                }
                LRESULT(0)
            }
            WM_TIMER if w.0 == TIMER_TICK => {
                if let Some(t) = c.hide_at {
                    if Instant::now() >= t {
                        c.rows.clear();
                        c.hide_at = None;
                        c.hot = None;
                        let _ = ShowWindow(h, SW_HIDE);
                    }
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                // 指针进到卡片上：取消收起，并算出悬停行做高亮
                c.hide_at = None;
                if !c.mouse_tracked {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: h,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    c.mouse_tracked = true;
                }
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                let hot = c.hit(y);
                if hot != c.hot {
                    c.hot = hot;
                    c.render(h);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                c.mouse_tracked = false;
                c.hot = None;
                c.hide_at = Some(Instant::now() + HIDE_GRACE);
                c.render(h);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                if let Some(i) = c.hit(y) {
                    if let Some(row) = c.rows.get(i).cloned() {
                        crate::tlog!(
                            "[wxhover] 点击直达 pid={} chat={:?}",
                            row.pid,
                            row.chat
                        );
                        crate::wechatmsg::open_session(row.pid, row.chat);
                    }
                }
                c.rows.clear();
                c.hide_at = None;
                c.hot = None;
                let _ = ShowWindow(h, SW_HIDE);
                LRESULT(0)
            }
            WM_NCHITTEST => {
                let pt = POINT {
                    x: (l.0 & 0xFFFF) as u16 as i16 as i32,
                    y: ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
                };
                let mut r = RECT::default();
                let _ = GetWindowRect(h, &mut r);
                let (ww, hh) = c.size;
                if inside_round(
                    (pt.x - r.left) as f32,
                    (pt.y - r.top) as f32,
                    ww as f32,
                    hh as f32,
                    16.0,
                ) {
                    LRESULT(HTCLIENT as isize)
                } else {
                    LRESULT(HTTRANSPARENT as isize)
                }
            }
            _ => DefWindowProcW(h, msg, w, l),
        }
    }
}

fn inside_round(x: f32, y: f32, w: f32, h: f32, r: f32) -> bool {
    if x < 0.0 || x > w || y < 0.0 || y > h {
        return false;
    }
    let r = r.min(w / 2.0).min(h / 2.0);
    let cx = if x < r {
        r
    } else if x > w - r {
        w - r
    } else {
        return true;
    };
    let cy = if y < r {
        r
    } else if y > h - r {
        h - r
    } else {
        return true;
    };
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

impl Hover {
    fn scale(&self, h: HWND) -> f32 {
        (unsafe { GetDpiForWindow(h) } as f32 / 96.0).max(0.75)
    }

    /// 客户端 y 坐标落在第几行（size 是物理像素，按 高/逻辑高 还原缩放）
    fn hit(&self, y: i32) -> Option<usize> {
        if self.size.1 <= 0 || self.rows.is_empty() {
            return None;
        }
        let scale = self.size.1 as f32 / self.logical_h();
        let ly = y as f32 / scale;
        for i in 0..self.rows.len() {
            let top = PAD + i as f32 * (ROW_H + ROW_GAP);
            if ly >= top && ly <= top + ROW_H {
                return Some(i);
            }
        }
        None
    }

    /// 逻辑高度（未乘 DPI）
    fn logical_h(&self) -> f32 {
        let n = self.rows.len().max(1) as f32;
        PAD * 2.0 + n * ROW_H + (n - 1.0) * ROW_GAP
    }

    fn accept(&mut self, h: HWND, rows: Vec<WxUnread>, anchor: (i32, i32)) {
        self.rows = rows.into_iter().take(MAX_ROWS).collect();
        self.anchor = anchor;
        self.hide_at = None;
        self.hot = None;
        if self.rows.is_empty() {
            let _ = unsafe { ShowWindow(h, SW_HIDE) };
            return;
        }
        self.place(h);
        self.render(h);
        let _ = unsafe { ShowWindow(h, SW_SHOWNOACTIVATE) };
    }

    fn place(&mut self, h: HWND) {
        let scale = self.scale(h);
        let w = (CARD_W * scale).round() as i32;
        let hh = (self.logical_h() * scale).round() as i32;
        self.size = (w, hh);
        let mon = crate::window::primary_monitor();
        let x = (self.anchor.0 - w / 2)
            .clamp(mon.rcMonitor.left + 8, (mon.rcMonitor.right - w - 8).max(mon.rcMonitor.left));
        let y = (self.anchor.1 - LIFT - hh).max(mon.rcMonitor.top + 8);
        unsafe {
            let _ = SetWindowPos(h, HWND_TOPMOST, x, y, w, hh, SWP_NOACTIVATE);
        }
    }

    fn render(&mut self, h: HWND) {
        let (w, hh) = self.size;
        if w <= 0 || hh <= 0 || self.rows.is_empty() {
            return;
        }
        let scale = self.scale(h);
        let rgba = paint(&mut self.view, w, hh, scale, &self.rows, self.hot);
        let bgra = render::premultiplied_rgba_to_bgra(&rgba);
        render::present(h, &bgra, w, hh);
    }
}

fn paint(
    soft: &mut SoftBackend,
    w: i32,
    h: i32,
    scale: f32,
    rows: &[WxUnread],
    hot: Option<usize>,
) -> Vec<u8> {
    soft.set_scale(scale);
    let fw = w as f32 / scale;
    let fh = h as f32 / scale;
    let ctx = soft.ctx().clone();
    let input = soft.input(fw, fh);
    let mut out = ctx.run_ui(input, |ui| {
        let p = ui.painter().clone();
        let t = cfg::cur();
        let board = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh));
        glass_board(&p, board, egui::CornerRadius::same(16));
        for (i, r) in rows.iter().enumerate() {
            let top = PAD + i as f32 * (ROW_H + ROW_GAP);
            let row = egui::Rect::from_min_size(
                egui::pos2(PAD, top),
                egui::vec2(fw - PAD * 2.0, ROW_H),
            );
            if hot == Some(i) {
                p.rect_filled(row, egui::CornerRadius::same(9), argb(t.hover));
            }
            // 左侧竖条：未读书多的红一点，免打扰的用暗色
            let bar_color = if r.muted { t.text_dim } else { t.accent };
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(row.left() + 2.0, row.top() + 12.0),
                    egui::vec2(3.0, ROW_H - 24.0),
                ),
                egui::CornerRadius::same(2),
                argb(bar_color),
            );
            // 会话名
            let name = ellipsize(soft, &r.chat, fw - PAD * 2.0 - 64.0, 16.0);
            txt(&p, row.left() + 14.0, row.top() + 10.0, &name, 16.0, t.text);
            // 摘要
            if !r.summary.is_empty() {
                let sub = ellipsize(soft, &r.summary, fw - PAD * 2.0 - 28.0, 13.0);
                txt(&p, row.left() + 14.0, row.top() + 32.0, &sub, 13.0, t.text_dim);
            }
            // 未读数徽标（右上）
            if r.unread > 0 {
                let label = if r.unread > 99 {
                    "99+".to_string()
                } else {
                    r.unread.to_string()
                };
                let bw = 22.0_f32.max(soft.text_width(&label, egui::FontId::proportional(12.0)) + 10.0);
                let bh = 18.0;
                let bx = row.right() - bw - 10.0;
                let by = row.top() + 9.0;
                let badge = egui::Rect::from_min_size(egui::pos2(bx, by), egui::vec2(bw, bh));
                p.rect_filled(badge, egui::CornerRadius::same(9), argb(if r.muted { t.text_dim } else { t.accent }));
                let tw = soft.text_width(&label, egui::FontId::proportional(12.0));
                txt(
                    &p,
                    bx + (bw - tw) / 2.0,
                    by + 1.0,
                    &label,
                    12.0,
                    0xFFFFFF,
                );
            }
        }
    });
    soft.update_textures(&mut out.textures_delta);
    soft.rasterize(out.shapes, out.pixels_per_point, w, h)
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

fn ellipsize(soft: &SoftBackend, text: &str, max_w: f32, size: f32) -> String {
    let font = egui::FontId::proportional(size);
    if soft.text_width(text, font.clone()) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut trial = out.clone();
        trial.push(ch);
        trial.push('…');
        if soft.text_width(&trial, font.clone()) > max_w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}
