//! 托盘测试程序：注册 N 个托盘图标（可多实例），点击时打印回调消息。
//! 只在开发 Orbit 的托盘镜像时用，不随主程序发布。

use std::ffi::c_void;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_SETVERSION, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyMenu, DestroyWindow, DispatchMessageW, GetMessageW, GetWindowLongPtrW, LoadCursorW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow,
    SetWindowLongPtrW, TrackPopupMenu, TranslateMessage, GWLP_USERDATA, ICONINFO, IDC_ARROW,
    HICON, MF_STRING, MSG, TPM_BOTTOMALIGN, TPM_LEFTALIGN, WM_APP, WM_CONTEXTMENU, WM_DESTROY,
    WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW,
};

const WM_TRAY: u32 = WM_APP + 1;
/// WM_MOUSEMOVE / WM_MOUSELEAVE（windows crate 没导出这两个名字）
const WM_MOUSEMOVE_MSG: u32 = 0x0200;
const WM_MOUSELEAVE_MSG: u32 = 0x02A3;
/// 通知图标 v4 的悬停通知（NIN_POPUPOPEN / NIN_POPUPCLOSE）
const NIN_POPUPOPEN: u32 = 0x0402;
const NIN_POPUPCLOSE: u32 = 0x0403;
const NOTIFYICON_VERSION_4: u32 = 4;
/// 悬停日志去抖：同一秒内 explorer 会连发好几条
static LAST_HOVER: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

struct Args {
    tag: String,
    count: u32,
    color: (u8, u8, u8),
    /// 日志前缀（可以和 tooltip 不同：tooltip 相同才能命中同一条 NotifyIconSettings）
    logtag: String,
}

unsafe fn make_icon(color: (u8, u8, u8)) -> HICON {
    let n = 32i32;
    let screen = GetDC(HWND::default());
    let mem = CreateCompatibleDC(screen);
    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: n,
        biHeight: -n,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: 0,
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let Ok(hbmp) = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), screen);
        return HICON::default();
    };
    let buf = std::slice::from_raw_parts_mut(bits as *mut u8, (n * n * 4) as usize);
    let r = n as f32 / 2.0;
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f32 + 0.5 - r, y as f32 + 0.5 - r);
            let d = (dx * dx + dy * dy).sqrt();
            let i = ((y * n + x) * 4) as usize;
            if d < r - 1.5 {
                buf[i] = color.2;
                buf[i + 1] = color.1;
                buf[i + 2] = color.0;
                buf[i + 3] = 255;
            } else if d < r {
                let a = (r - d) as u8;
                buf[i] = color.2;
                buf[i + 1] = color.1;
                buf[i + 2] = color.0;
                buf[i + 3] = a.saturating_mul(255);
            }
        }
    }
    let _ = SelectObject(mem, hbmp);
    let mask: HBITMAP = CreateBitmap(n, n, 1, 1, None);
    let info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: hbmp,
    };
    let icon = CreateIconIndirect(&info).unwrap_or_default();
    let _ = DeleteObject(hbmp);
    let _ = DeleteObject(mask);
    let _ = DeleteDC(mem);
    ReleaseDC(HWND::default(), screen);
    icon
}

unsafe fn add_icon(hwnd: HWND, uid: u32, tip: &str, icon: HICON) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: uid,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    let t: Vec<u16> = tip.encode_utf16().take(127).collect();
    nid.szTip[..t.len()].copy_from_slice(&t);
    let ok = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
    // 切到 v4：悬停时 shell 会发 NIN_POPUPOPEN（老版本是 WM_MOUSEMOVE）
    let mut ver = nid;
    ver.Anonymous.uTimeout = NOTIFYICON_VERSION_4;
    let v = Shell_NotifyIconW(NIM_SETVERSION, &ver).as_bool();
    println!("[{tip}] NIM_ADD ok={ok} v4={v}");
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        let tag = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Args;
        if msg == WM_TRAY && !tag.is_null() {
            let ev = l.0 as u32 & 0xFFFF;
            if ev == WM_LBUTTONUP || ev == WM_RBUTTONUP {
                println!(
                    "[{}] click uid={} {}",
                    { let a = &*tag; if a.logtag.is_empty() { &a.tag } else { &a.logtag } },
                    w.0,
                    if ev == WM_RBUTTONUP { "R" } else { "L" }
                );
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
            if ev == NIN_POPUPOPEN || ev == NIN_POPUPCLOSE {
                println!(
                    "[{}] popup uid={} {}",
                    { let a = &*tag; if a.logtag.is_empty() { &a.tag } else { &a.logtag } },
                    (l.0 >> 16) & 0xFFFF,
                    if ev == NIN_POPUPOPEN { "open" } else { "close" }
                );
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
            if ev == WM_MOUSEMOVE_MSG || ev == WM_MOUSELEAVE_MSG {
                // 悬停通知：explorer 在指针真的进到图标上时才会发
                let mut g = LAST_HOVER.lock().unwrap();
                let now = std::time::Instant::now();
                if g.map_or(true, |t| now.duration_since(t).as_millis() > 700) {
                    *g = Some(now);
                    println!(
                        "[{}] hover uid={} msg=0x{:X}",
                        { let a = &*tag; if a.logtag.is_empty() { &a.tag } else { &a.logtag } },
                        w.0,
                        ev
                    );
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
            }
            if ev == WM_RBUTTONUP {
                if let Ok(menu) = CreatePopupMenu() {
                    let _ = AppendMenuW(menu, MF_STRING, 1, w!("测试菜单项"));
                    let mut pt = POINT::default();
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
                    let _ = SetForegroundWindow(hwnd);
                    let _ = TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, None);
                    let _ = DestroyMenu(menu);
                }
            }
            return LRESULT(0);
        }
        if msg == WM_CONTEXTMENU {
            return LRESULT(0);
        }
        if msg == WM_DESTROY {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, w, l)
    }
}

fn main() {
    let mut args = Args { tag: "test".into(), count: 1, color: (230, 60, 60), logtag: String::new() };
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--tag" if i + 1 < argv.len() => {
                args.tag = argv[i + 1].clone();
                i += 2;
            }
            "--count" if i + 1 < argv.len() => {
                args.count = argv[i + 1].parse().unwrap_or(1);
                i += 2;
            }
            "--logtag" if i + 1 < argv.len() => {
                args.logtag = argv[i + 1].clone();
                i += 2;
            }
            "--cyan" => {
                args.color = (40, 200, 220);
                i += 1;
            }
            "--green" => {
                args.color = (60, 200, 90);
                i += 1;
            }
            _ => i += 1,
        }
    }

    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("TrayTestWnd"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let args = Box::into_raw(Box::new(args));
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("TrayTestWnd"),
            w!("traytest"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            200,
            100,
            HWND::default(),
            None,
            hinstance,
            None,
        )
        .unwrap();
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, args as isize);
        let a = &*args;
        let icon = make_icon(a.color);
        for k in 0..a.count {
            let uid = k + 1;
            let tip = if a.count > 1 {
                format!("{} #{}", a.tag, uid)
            } else {
                a.tag.clone()
            };
            add_icon(hwnd, uid, &tip, icon);
        }
        // TaskbarCreated：explorer 重启后重新注册
        let created = RegisterWindowMessageW(w!("TaskbarCreated"));
        let _ = created;

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
