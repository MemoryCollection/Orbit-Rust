//! 邮件阅读窗：点邮件标题弹出。用普通 popup 窗而不是分层面板（UpdateLayeredWindow
//! 带不了可见的子控件），只读多行 EDIT 呈现正文：文本可选中、可 Ctrl+C 复制、
//! 可滚动。「删除邮件」按钮经 WM_COPYDATA 把 UID 同步发回胶囊窗口，由它调
//! `Mail::delete`（本地移除 + 后台 `\Deleted` + EXPUNGE 真删）。

use std::cell::Cell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, HDC, HBRUSH, HFONT, CLIP_DEFAULT_PRECIS,
    CLEARTYPE_QUALITY, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE, FW_BOLD, FW_NORMAL,
    OUT_DEFAULT_PRECIS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging as wm;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetWindowLongPtrW, LoadCursorW,
    RegisterClassW, SendMessageW, SetWindowLongPtrW, SetWindowPos, ShowWindow, BS_PUSHBUTTON,
    CREATESTRUCTW, CW_USEDEFAULT, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY,
    GWLP_USERDATA, HMENU, HWND_TOPMOST, IDC_ARROW, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE,
    WM_COMMAND, WM_COPYDATA, WM_CREATE, WM_CTLCOLORSTATIC, WM_CTLCOLOREDIT, WM_SETFONT,
    WM_SETTEXT, WM_VSCROLL, WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VSCROLL,
    WNDCLASSW,
};

/// 删除按钮经 WM_COPYDATA 发 UID 时用的 dwData 魔数（"MAIL"）
pub const MAIL_DELETE_MAGIC: usize = 0x4D41_494C;

/// 深色主题（COLORREF = 0x00BBGGRR）
const BG: COLORREF = COLORREF(0x002A221E); // RGB(30,34,42)
const FG: COLORREF = COLORREF(0x00EBE9E8); // RGB(232,233,235)

// 子控件 ID（HMENU 低 16 位）
const ID_DELETE: isize = 2;
const ID_CLOSE: isize = 3;

struct Reader {
    owner: HWND,
    uid: String,
    scale: f32,
}

thread_local! {
    static READER_HWND: Cell<isize> = const { Cell::new(0) };
    static BG_BRUSH: Cell<usize> = const { Cell::new(0) };
    /// (subject, from, edit) 子控件句柄；按钮按 ID 走 WM_COMMAND，不需要句柄
    static CHILDREN: Cell<Option<(isize, isize, isize)>> = const { Cell::new(None) };
}

fn hwnd() -> HWND {
    HWND(READER_HWND.with(|c| c.get()) as *mut _)
}

/// 逻辑值 → 物理像素
fn s(scale: f32, v: f32) -> f32 {
    v * scale
}

/// 初始化：注册类并建主窗（隐藏）。子控件在首次 show 时创建，之后复用。
pub fn init(instance: HINSTANCE) {
    unsafe {
        let brush = CreateSolidBrush(BG);
        BG_BRUSH.with(|c| c.set(brush.0 as usize));
        let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: arrow,
            hbrBackground: brush,
            style: wm::CS_HREDRAW | wm::CS_VREDRAW,
            lpszClassName: w!("ORBIT_MAILVIEW"),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let p = Box::into_raw(Box::new(Reader {
            owner: HWND::default(),
            uid: String::new(),
            scale: 1.0,
        }));
        match CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("ORBIT_MAILVIEW"),
            PCWSTR::null(),
            WS_POPUP,
            CW_USEDEFAULT, CW_USEDEFAULT, 10, 10,
            HWND::default(),
            HMENU::default(),
            instance,
            Some(p as *const core::ffi::c_void),
        ) {
            Ok(h) => READER_HWND.with(|c| c.set(h.0 as isize)),
            Err(_) => drop(Box::from_raw(p)),
        }
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

/// 显示/更新阅读窗。(x, y) = 期望左上角（物理像素），自动收进工作区。
pub fn show(
    owner: HWND, scale: f32, subject: &str, from: &str, body: &str, uid: &str, x: i32, y: i32,
) {
    let h = hwnd();
    if h.is_invalid() {
        return;
    }
    unsafe {
        let ptr = GetWindowLongPtrW(h, GWLP_USERDATA) as *mut Reader;
        if ptr.is_null() {
            return;
        }
        let p = &mut *ptr;
        p.owner = owner;
        p.uid = uid.to_string();
        p.scale = scale;

        let (w, hh) = (s(scale, 500.0) as i32, s(scale, 560.0) as i32);
        let wa = crate::window::primary_monitor().rcWork;
        let px = x.clamp(wa.left + 4, (wa.right - w - 4).max(wa.left + 4));
        let py = y.clamp(wa.top + 4, (wa.bottom - hh - 4).max(wa.top + 4));
        let _ = SetWindowPos(h, HWND_TOPMOST, px, py, w, hh, SWP_NOACTIVATE);
        crate::tlog!("[mailview] show uid={} at ({px},{py}) {w}x{hh}", uid, );

        ensure_children(h, scale);

        // 填内容（WM_SETTEXT，结尾要 NUL）
        let set_text = |ctrl: HWND, text: &str| {
            let mut t: Vec<u16> = text.encode_utf16().collect();
            t.push(0);
            SendMessageW(ctrl, WM_SETTEXT, WPARAM(0), LPARAM(t.as_ptr() as isize));
        };
        if let Some((sub, frm, edit)) = CHILDREN.with(|c| c.get()) {
            set_text(HWND(sub as *mut _), subject);
            set_text(HWND(frm as *mut _), &format!("发件人：{from}"));
            let body = if body.is_empty() { "（无正文）" } else { body };
            set_text(HWND(edit as *mut _), body);
            // 滚动回顶部并给正文焦点（可选中、可 Ctrl+C）
            SendMessageW(HWND(edit as *mut _), WM_VSCROLL, WPARAM(6 << 16), LPARAM(0));
            let _ = SetFocus(HWND(edit as *mut _));
        }

        let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
    }
}

/// 首次创建子控件（只建一次，之后复用）
fn ensure_children(h: HWND, scale: f32) {
    if CHILDREN.with(|c| c.get().is_some()) {
        return;
    }
    unsafe {
        let (w, hh) = (s(scale, 500.0), s(scale, 560.0));
        let pad = s(scale, 16.0);
        let font_sub = create_font(-(s(scale, 17.0) as i32), true);
        let font_txt = create_font(-(s(scale, 13.0) as i32), false);
        let font_btn = create_font(-(s(scale, 14.0) as i32), false);

        let mk = |cls: PCWSTR, text: &[u16], style: wm::WINDOW_STYLE, x: f32, y: f32, cw: f32, ch: f32, id: isize| {
            let mut t = text.to_vec();
            t.push(0);
            CreateWindowExW(
                wm::WS_EX_NOPARENTNOTIFY,
                cls,
                PCWSTR(t.as_ptr()),
                style | WS_CHILD | wm::WS_VISIBLE,
                x as i32, y as i32, cw as i32, ch as i32,
                h,
                HMENU(id as *mut _),
                None,
                None,
            )
        };

        let sub = mk(w!("STATIC"), &[], wm::WINDOW_STYLE(0), pad, s(scale, 14.0), w - 2.0 * pad, s(scale, 30.0), 10);
        let frm = mk(w!("STATIC"), &[], wm::WINDOW_STYLE(0), pad, s(scale, 50.0), w - 2.0 * pad, s(scale, 20.0), 11);
        let edit = mk(
            w!("EDIT"), &[],
            wm::WINDOW_STYLE(((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32) | WS_VSCROLL.0),
            pad, s(scale, 78.0), w - 2.0 * pad, hh - s(scale, 134.0), 1,
        );
        let bw = s(scale, 110.0);
        let by = hh - s(scale, 44.0);
        let del = mk(
            w!("BUTTON"), w!("删除邮件").as_wide(), wm::WINDOW_STYLE(BS_PUSHBUTTON as u32),
            pad, by, bw, s(scale, 32.0), ID_DELETE,
        );
        let close = mk(
            w!("BUTTON"), w!("关 闭").as_wide(), wm::WINDOW_STYLE(BS_PUSHBUTTON as u32),
            w - pad - bw, by, bw, s(scale, 32.0), ID_CLOSE,
        );

        // 控件字体：建完后补发 WM_SETFONT（粗体 = 主题标题）
        let apply_font = |ctrl: HWND, fnt: HFONT, repaint: bool| {
            SendMessageW(ctrl, WM_SETFONT, WPARAM(fnt.0 as usize), LPARAM(repaint as isize));
        };
        apply_font(sub.as_ref().copied().unwrap_or_default(), font_sub, true);
        apply_font(frm.as_ref().copied().unwrap_or_default(), font_txt, true);
        apply_font(edit.as_ref().copied().unwrap_or_default(), font_txt, true);
        if let Ok(d) = del { apply_font(d, font_btn, true); }
        if let Ok(c) = close { apply_font(c, font_btn, true); }

        CHILDREN.with(|c| {
            c.set(Some((
                sub.map(|h| h.0 as isize).unwrap_or(0),
                frm.map(|h| h.0 as isize).unwrap_or(0),
                edit.map(|h| h.0 as isize).unwrap_or(0),
            )))
        });
    }
}

fn create_font(px: i32, bold: bool) -> HFONT {
    unsafe {
        CreateFontW(
            px, 0, 0, 0,
            if bold { FW_BOLD.0 as i32 } else { FW_NORMAL.0 as i32 },
            0, 0, 0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            w!("Microsoft YaHei UI"),
        )
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CREATE {
            let cs = *(l.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(h, GWLP_USERDATA, cs.lpCreateParams as isize);
            return LRESULT(0);
        }
        let ptr = GetWindowLongPtrW(h, GWLP_USERDATA) as *mut Reader;
        if ptr.is_null() {
            return DefWindowProcW(h, msg, w, l);
        }
        let p = &mut *ptr;
        match msg {
            // 深色主题：编辑框与静态文本统一配色
            WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
                let hdc = HDC(w.0 as *mut core::ffi::c_void);
                let _ = windows::Win32::Graphics::Gdi::SetTextColor(hdc, FG);
                let _ = windows::Win32::Graphics::Gdi::SetBkColor(hdc, BG);
                let brush = HBRUSH(BG_BRUSH.with(|c| c.get()) as *mut core::ffi::c_void);
                LRESULT(brush.0 as isize)
            }
            WM_COMMAND => {
                let id = (w.0 & 0xFFFF) as isize;
                match id {
                    ID_CLOSE => {
                        let _ = ShowWindow(h, SW_HIDE);
                        LRESULT(0)
                    }
                    ID_DELETE => {
                        // 把 UID 原样交给胶囊窗口：它调 Mail::delete（本地 + 服务器）
                        let owner = p.owner;
                        let data: Vec<u16> = p.uid.encode_utf16().collect();
                        let cds = COPYDATASTRUCT {
                            dwData: MAIL_DELETE_MAGIC,
                            cbData: (data.len() * 2) as u32,
                            lpData: data.as_ptr() as *mut core::ffi::c_void,
                        };
                        if !owner.is_invalid() && !data.is_empty() {
                            SendMessageW(
                                owner,
                                WM_COPYDATA,
                                WPARAM(h.0 as usize),
                                LPARAM(&cds as *const COPYDATASTRUCT as isize),
                            );
                        }
                        let _ = ShowWindow(h, SW_HIDE);
                        LRESULT(0)
                    }
                    _ => LRESULT(0),
                }
            }
            _ => DefWindowProcW(h, msg, w, l),
        }
    }
}
