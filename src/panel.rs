//! 展开面板：胶囊下方的小弹窗（天气宽卡片 / 剪贴板历史 / 邮件列表）。
//! 独立分层窗口，不抢焦点；点击时把「动作号」PostMessage 回胶囊窗口，由它决定
//! 复制/已读/固定/切标签以及是否收起，面板自身不做业务判断。
//! 显隐由胶囊窗口驱动（App::tick_panel 判断鼠标是否离开），面板自身不做定时器。

use std::cell::Cell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetWindowLongPtrW, GetWindowRect, IsWindowVisible, LoadCursorW,
    PostMessageW, RegisterClassW, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW,
    GWLP_USERDATA, HMENU, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW, SWP_NOACTIVATE, SW_HIDE,
    SW_SHOWNOACTIVATE, WM_CREATE, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCHITTEST, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WNDCLASSW,
};

use crate::render::{self, PanelRow, WeatherView};

/// 面板动作被点击时发给胶囊窗口的消息（wParam = 动作号）
pub const WM_ROW_CLICK: u32 = 0x8000 + 1;

/// 面板内容：通用列表（剪贴板 / 邮件）或天气宽卡片
pub enum Content {
    List {
        title: String,
        subtitle: String,
        /// 右上标签页（空 = 不显示）
        tabs: Vec<String>,
        tab_active: usize,
        rows: Vec<PanelRow>,
        empty: String,
    },
    Weather(Box<WeatherView>),
}

impl Content {
    fn size(&self, scale: f32) -> (i32, i32) {
        match self {
            Content::List { title, subtitle, rows, empty, tabs, .. } => {
                render::panel_size(scale, title, subtitle, rows, empty, tabs)
            }
            Content::Weather(v) => render::weather_panel_size(scale, v),
        }
    }
}

struct Panel {
    owner: HWND,
    scale: f32,
    content: Content,
    /// 命中区域与动作号（后压入的优先，固定按钮压在行上面）
    hits: Vec<([f32; 4], u32)>,
    hovered: Option<u32>,
    mouse_tracked: bool,
    size: (i32, i32),
}

thread_local! {
    static PANEL_HWND: Cell<isize> = const { Cell::new(0) };
}

pub fn init(instance: windows::Win32::Foundation::HINSTANCE) {
    unsafe {
        let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: arrow,
            lpszClassName: w!("ORBIT_PANEL"),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let p = Box::into_raw(Box::new(Panel {
            owner: HWND::default(),
            scale: 1.0,
            content: Content::List {
                title: String::new(),
                subtitle: String::new(),
                tabs: vec![],
                tab_active: 0,
                rows: vec![],
                empty: String::new(),
            },
            hits: Vec::new(),
            hovered: None,
            mouse_tracked: false,
            size: (0, 0),
        }));
        match CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("ORBIT_PANEL"),
            PCWSTR::null(),
            WS_POPUP,
            0, 0, 10, 10,
            HWND::default(),
            HMENU::default(),
            instance,
            Some(p as *const core::ffi::c_void),
        ) {
            Ok(h) => PANEL_HWND.with(|c| c.set(h.0 as isize)),
            Err(_) => drop(Box::from_raw(p)),
        }
    }
}

fn hwnd() -> HWND {
    HWND(PANEL_HWND.with(|c| c.get()) as *mut _)
}

/// 在 (anchor_x 居中, top_y) 处显示面板；x 会自动收进工作区
pub fn show(owner: HWND, scale: f32, content: Content, anchor_x: i32, top_y: i32) {
    let h = hwnd();
    if h.is_invalid() {
        return;
    }
    let (w, hh) = content.size(scale);
    unsafe {
        let ptr = GetWindowLongPtrW(h, GWLP_USERDATA) as *mut Panel;
        if ptr.is_null() {
            return;
        }
        let p = &mut *ptr;
        p.owner = owner;
        p.scale = scale;
        p.hovered = None;
        p.size = (w, hh);
        p.content = content;

        let wa = crate::window::primary_monitor().rcWork;
        let x = (anchor_x - w / 2).clamp(wa.left + 4, (wa.right - w - 4).max(wa.left + 4));
        let _ = SetWindowPos(h, HWND_TOPMOST, x, top_y, w, hh, SWP_NOACTIVATE);
        p.render();
        let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
    }
}

pub fn hide() {
    let h = hwnd();
    if !h.is_invalid() {
        unsafe {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
}

pub fn visible() -> bool {
    let h = hwnd();
    if h.is_invalid() {
        return false;
    }
    unsafe { IsWindowVisible(h).as_bool() }
}

/// 面板当前是否包含屏幕点 pt
pub fn contains(pt: POINT) -> bool {
    let h = hwnd();
    if h.is_invalid() || !visible() {
        return false;
    }
    unsafe {
        let mut r = RECT::default();
        if GetWindowRect(h, &mut r).is_err() {
            return false;
        }
        pt.x >= r.left && pt.x <= r.right && pt.y >= r.top && pt.y <= r.bottom
    }
}

impl Panel {
    fn render(&mut self) {
        let (w, h) = self.size;
        if w <= 0 || h <= 0 {
            return;
        }
        let out = match &self.content {
            Content::List { title, subtitle, rows, empty, tabs, tab_active } => render::draw_panel(
                w, h, self.scale, title, subtitle, rows, empty, tabs, *tab_active, self.hovered,
            ),
            Content::Weather(v) => {
                render::draw_weather_panel(w, h, self.scale, v, self.hovered == Some(0))
            }
        };
        self.hits = out.hits;
        let bgra = render::to_premultiplied_bgra(&out.rgba);
        render::present(hwnd(), &bgra, w, h);
    }

    /// 命中的动作号（后压入的区域优先，固定按钮压在行上面）
    fn action_at(&self, x: f32, y: f32) -> Option<u32> {
        self.hits
            .iter()
            .rev()
            .find(|(r, _)| x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3])
            .map(|(_, id)| *id)
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CREATE {
            let cs = *(l.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(h, GWLP_USERDATA, cs.lpCreateParams as isize);
            return LRESULT(0);
        }
        let ptr = GetWindowLongPtrW(h, GWLP_USERDATA) as *mut Panel;
        if ptr.is_null() {
            return DefWindowProcW(h, msg, w, l);
        }
        let p = &mut *ptr;
        match msg {
            WM_NCHITTEST => {
                let mut pt = POINT {
                    x: (l.0 & 0xFFFF) as u16 as i16 as i32,
                    y: ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
                };
                let mut wr = RECT::default();
                let _ = GetWindowRect(h, &mut wr);
                pt.x -= wr.left;
                pt.y -= wr.top;
                let (pw, ph) = p.size;
                if inside_round(pt.x as f32, pt.y as f32, pw as f32, ph as f32, crate::config::PANEL_RADIUS * p.scale) {
                    LRESULT(HTCLIENT as isize)
                } else {
                    LRESULT(HTTRANSPARENT as isize)
                }
            }
            WM_MOUSEMOVE => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                let id = p.action_at(x as f32, y as f32);
                if id != p.hovered {
                    p.hovered = id;
                    p.render();
                }
                if !p.mouse_tracked {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: h,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    p.mouse_tracked = true;
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                p.mouse_tracked = false;
                if p.hovered.take().is_some() {
                    p.render();
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                if let Some(id) = p.action_at(x as f32, y as f32) {
                    let owner = p.owner;
                    if !owner.is_invalid() {
                        let _ = PostMessageW(owner, WM_ROW_CLICK, WPARAM(id as usize), LPARAM(0));
                    }
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(h, msg, w, l),
        }
    }
}

/// 圆角矩形命中
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
