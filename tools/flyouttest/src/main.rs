//! 离线验证：`flyout.rs` 到底把"托盘悬停弹窗"摆到哪了。
//!
//! 做法：派生一个子进程造出"应用的悬停弹窗"（WS_POPUP + WS_EX_TOOLWINDOW，
//! 初始摆在旧托盘角 1500,900，320x420），再调用产品代码 `flyout::watch()`
//! 看它有没有被挪到「鼠标 x 居中 / 下沿贴胶囊上沿」。
//! 不依赖微信、不需要真的悬停托盘，也不碰任务栏。
//!
//! 用法：flyouttest [anchor_x]   默认 1043（= Dock 右半边的某个图标）

#![allow(dead_code)]

// flyout.rs 里用 `crate::tlog!` / `crate::vlog!`，这里给最简实现
#[macro_export]
macro_rules! tlog {
    ($($t:tt)*) => { println!("[tlog] {}", format_args!($($t)*)) };
}
#[macro_export]
macro_rules! vlog {
    ($($t:tt)*) => { println!("[vlog] {}", format_args!($($t)*)) };
}

#[path = "../../../src/flyout.rs"]
mod flyout;

use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetMessageW, GetWindowRect,
    RegisterClassW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HMENU, MSG,
    SW_SHOWNOACTIVATE, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, m, w, l) }
}

/// 子进程：造一个和"应用悬停弹窗"同类型的窗口，摆在旧托盘角
fn run_popup() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let cls = w!("FT_POPUP");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: cls,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let h = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            cls,
            w!("悬停预览"),
            WS_POPUP,
            1500,
            900,
            320,
            420,
            HWND::default(),
            HMENU::default(),
            hinst,
            None,
        )
        .unwrap_or_default();
        if h.0.is_null() {
            println!("子进程建窗失败");
            return;
        }
        let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn rect_of(h: HWND) -> (i32, i32, i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(h, &mut r);
    }
    (r.left, r.top, r.right - r.left, r.bottom - r.top)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s == "popup").unwrap_or(false) {
        run_popup();
        return;
    }
    let anchor_x: i32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1043);

    // 真机 1.0 缩放下 Dock 的屏幕矩形（宽度会随应用数变，这里取一段固定值即可）
    let capsule = RECT { left: 777, top: 1016, right: 1142, bottom: 1072 };

    let exe = std::env::current_exe().expect("current_exe");
    let mut child = std::process::Command::new(exe)
        .arg("popup")
        .spawn()
        .expect("spawn popup");

    // 等子进程的窗口出现
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut hwnd = HWND::default();
    while Instant::now() < deadline {
        let h = unsafe { FindWindowW(w!("FT_POPUP"), PCWSTR::null()) }.unwrap_or_default();
        if !h.0.is_null() {
            hwnd = h;
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    if hwnd.0.is_null() {
        println!("找不到子进程窗口");
        let _ = child.kill();
        return;
    }

    let (x0, y0, w, h) = rect_of(hwnd);
    println!("初始位置 ({x0},{y0}) {w}x{h}");

    flyout::watch(child.id(), HWND::default(), capsule, anchor_x, "测试弹窗", false);
    std::thread::sleep(Duration::from_millis(1200));

    let (x1, y1, w1, h1) = rect_of(hwnd);
    // 产品代码会把锚点先夹进 Dock 区间（光标不可能停在 Dock 外面）
    let anchor = anchor_x.clamp(capsule.left, capsule.right);
    let want_x = anchor - w / 2;
    let want_y = capsule.top - h - 1;
    println!("摆放后 ({x1},{y1}) {w1}x{h1}");
    println!("期望   ({want_x},{want_y})   ← 鼠标 x={anchor} 居中、下沿贴胶囊上沿 y={}", capsule.top);
    println!("结果   x:{}, y:{}", if (x1 - want_x).abs() <= 2 { "OK" } else { "错" }, if (y1 - want_y).abs() <= 2 { "OK" } else { "错" });

    let _ = child.kill();
}
