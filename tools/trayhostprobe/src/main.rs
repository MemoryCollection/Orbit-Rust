//! 托盘托管验证探针。
//!
//! 复刻 ManagedShell 的做法：注册一个假的 `Shell_TrayWnd` / `TrayNotifyWnd`
//! 窗口并广播 `TaskbarCreated`，看系统/应用会不会把托盘注册消息（WM_COPYDATA）
//! 发给我们。用于确认"接管托盘"这条路在本机可行，并抓原始字节布局。
//!
//! 用法：trayhostprobe [秒数，默认 20]

use std::ffi::c_void;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, PM_REMOVE, RegisterClassW,
    RegisterWindowMessageW, SendNotifyMessageW, TranslateMessage, WINDOW_EX_STYLE, WNDCLASSW,
    WM_COPYDATA, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_TOPMOST, WS_EX_TOOLWINDOW,
    WS_POPUP, HWND_BROADCAST,
};

const TRAY_CLASS: &str = "Shell_TrayWnd";
const NOTIFY_CLASS: &str = "TrayNotifyWnd";

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    println!("=== trayhostprobe 启动，监听 {secs}s ===");

    // restore 模式：只重播 TaskbarCreated，让应用把图标重新注册回真任务栏
    if std::env::args().any(|a| a == "restore") {
        unsafe {
            let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
            let _ = SendNotifyMessageW(HWND_BROADCAST, msg, WPARAM(0), LPARAM(0));
        }
        println!("已重播 TaskbarCreated");
        return;
    }

    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();

        let tray_cls = register(tray_wndproc, TRAY_CLASS, hinst);
        let notify_cls = register(notify_wndproc, NOTIFY_CLASS, hinst);
        println!("RegisterClass: tray={tray_cls:#x} notify={notify_cls:#x}");

        let htray = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(tray_cls as *const u16),
            w!(""),
            WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            0,
            1920,
            23,
            HWND::default(),
            None,
            hinst,
            None,
        )
        .expect("create Shell_TrayWnd");
        let hnotify = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(notify_cls as *const u16),
            PCWSTR::null(),
            WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            0,
            1920,
            23,
            htray,
            None,
            hinst,
            None,
        )
        .expect("create TrayNotifyWnd");
        println!("HWND: tray={:?} notify={:?}", htray.0, hnotify.0);

        let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
        println!("TaskbarCreated = {msg:#x}, 广播...");
        let _ = SendNotifyMessageW(HWND_BROADCAST, msg, WPARAM(0), LPARAM(0));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            if std::time::Instant::now() >= deadline {
                break;
            }
            let mut m = std::mem::zeroed();
            if PeekMessageW(&mut m, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&m);
                let _ = DispatchMessageW(&m);
            } else {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        println!("=== 结束 ===");
    }
}

unsafe fn register(
    proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    name: &str,
    hinst: HINSTANCE,
) -> u16 {
    let mut wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let wc = WNDCLASSW {
        style: windows::Win32::UI::WindowsAndMessaging::WNDCLASS_STYLES(0x8),
        lpfnWndProc: Some(proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
        hCursor: windows::Win32::UI::WindowsAndMessaging::HCURSOR::default(),
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(wide.as_mut_ptr()),
    };
    RegisterClassW(&wc)
}

unsafe extern "system" fn tray_wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        dump_copydata(l);
        return LRESULT(1);
    }
    if msg >= 0x0400 {
        println!("[tray] WM_USER+ msg={msg:#x} w={:#x} l={:#x}", w.0, l.0);
    }
    DefWindowProcW(h, msg, w, l)
}

unsafe extern "system" fn notify_wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        println!("[notify] 收到 WM_COPYDATA！");
        dump_copydata(l);
        return LRESULT(1);
    }
    DefWindowProcW(h, msg, w, l)
}

unsafe fn dump_copydata(l: LPARAM) {
    if l.0 == 0 {
        return;
    }
    let cd = &*(l.0 as *const COPYDATASTRUCT);
    println!(
        "[copydata] dwData={} cbData={} lpData={:#x}",
        cd.dwData, cd.cbData, cd.lpData as usize
    );
    if cd.lpData.is_null() || cd.cbData == 0 {
        return;
    }
    let n = (cd.cbData as usize).min(0x420);
    let bytes = std::slice::from_raw_parts(cd.lpData as *const u8, n);
    print!("[copydata] u32[0..{}]: ", (n / 4).min(40));
    for i in 0..(n / 4).min(40) {
        let v = u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        print!("{v:#010x} ");
    }
    println!();
    if cd.dwData == 1 && n >= 16 {
        let dw_msg = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let cb = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        println!("[copydata]   NIM dwMessage={dw_msg:#x} nid.cbSize={cb:#x}({cb})",);
    }
}
