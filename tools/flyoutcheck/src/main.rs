//! 验证 `src/flyout.rs`：一个进程开个 tool window 当"悬停弹窗"，另一份进程触发重定位。

#[macro_export]
macro_rules! tlog {
    ($($t:tt)*) => { println!($($t)*) };
}

#[macro_export]
macro_rules! vlog {
    ($($t:tt)*) => { println!($($t)*) };
}

#[path = "../../../src/flyout.rs"]
mod flyout;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

unsafe extern "system" fn proc_(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, m, w, l) }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(|s| s.as_str()) {
        Some("popup") => {
            let (x, y): (i32, i32) = (a[2].parse().unwrap(), a[3].parse().unwrap());
            let (w_, h_): (i32, i32) = (a[4].parse().unwrap(), a[5].parse().unwrap());
            unsafe {
                let cls = w!("OrbitFlyoutTest");
                let hinst = GetModuleHandleW(PCWSTR::null()).unwrap();
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(proc_),
                    hInstance: hinst.into(),
                    lpszClassName: cls,
                    ..Default::default()
                };
                RegisterClassW(&wc);
                let h = CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    cls,
                    w!("悬停弹窗"),
                    WS_POPUP | WS_VISIBLE,
                    x, y, w_, h_,
                    None, None, hinst, None,
                )
                .unwrap();
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                println!("popup pid={} hwnd={:#x} @({x},{y}) {w_}x{h_}", std::process::id(), h.0 as usize);
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
            }
        }
        Some("watch") => {
            let pid: u32 = a[2].parse().unwrap();
            let cap = RECT {
                left: a[3].parse().unwrap(),
                top: a[4].parse().unwrap(),
                right: a[5].parse().unwrap(),
                bottom: a[6].parse().unwrap(),
            };
            flyout::watch(pid, cap, "测试图标");
            std::thread::sleep(std::time::Duration::from_millis(7000));
        }
        Some("realmove") => {
            // 真·鼠标输入：SendInput 绝对坐标（会生成 WM_MOUSEMOVE，SetCursorPos 不会）
            use windows::Win32::UI::Input::KeyboardAndMouse::{
                SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE,
                MOUSEINPUT,
            };
            let (x, y): (i32, i32) = (a[2].parse().unwrap(), a[3].parse().unwrap());
            let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(1);
            let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(1);
            let nx = (x as f64 * 65535.0 / (sw - 1) as f64) as i32;
            let ny = (y as f64 * 65535.0 / (sh - 1) as f64) as i32;
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: nx,
                        dy: ny,
                        mouseData: 0,
                        dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            let n = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
            println!("SendInput {n} → ({x},{y})");
        }
        Some("whatson") => {
            let (x, y): (i32, i32) = (a[2].parse().unwrap(), a[3].parse().unwrap());
            unsafe {
                let h = WindowFromPoint(windows::Win32::Foundation::POINT { x, y });
                let mut p = 0u32;
                GetWindowThreadProcessId(h, Some(&mut p));
                let mut b = [0u16; 128];
                let n = GetClassNameW(h, &mut b);
                println!(
                    "({x},{y}) → hwnd={:#x} class={} pid={p}",
                    h.0 as usize,
                    String::from_utf16_lossy(&b[..n as usize])
                );
            }
        }
        Some("snap") => {
            unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
                let v = unsafe { &mut *(l.0 as *mut Vec<String>) };
                let mut p = 0u32;
                unsafe { GetWindowThreadProcessId(h, Some(&mut p)) };
                let mut b = [0u16; 128];
                let n = unsafe { GetClassNameW(h, &mut b) };
                let mut r = RECT::default();
                unsafe {
                    if GetWindowRect(h, &mut r).is_err() {
                        return BOOL(1);
                    }
                };
                if unsafe { IsWindowVisible(h).as_bool() } {
                    v.push(format!(
                        "{} {} ({},{},{},{}) pid={}",
                        h.0 as usize,
                        String::from_utf16_lossy(&b[..n as usize]),
                        r.left, r.top, r.right, r.bottom, p
                    ));
                }
                BOOL(1)
            }
            let mut all: Vec<String> = Vec::new();
            unsafe {
                let _ = EnumWindows(Some(cb), LPARAM(&mut all as *mut _ as isize));
            }
            all.sort();
            for l in all {
                println!("{l}");
            }
        }
        Some("hoverhold") => {
            let (x, y): (i32, i32) = (a[2].parse().unwrap(), a[3].parse().unwrap());
            let ms: u64 = a[4].parse().unwrap();
            unsafe {
                let mut old = windows::Win32::Foundation::POINT::default();
                let _ = GetCursorPos(&mut old);
                let _ = SetCursorPos(x, y);
                std::thread::sleep(std::time::Duration::from_millis(ms));
                let _ = SetCursorPos(old.x, old.y);
            }
            println!("按住 {ms}ms 结束");
        }
        Some("moveto") => {
            let (x, y): (i32, i32) = (a[2].parse().unwrap(), a[3].parse().unwrap());
            unsafe {
                let _ = SetCursorPos(x, y);
            }
            println!("光标 → ({x},{y})");
        }
        Some("wins") => {
            let pid: u32 = a[2].parse().unwrap();
            unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
                let v = unsafe { &mut *(l.0 as *mut Vec<(u32, HWND)>) };
                let mut p = 0u32;
                unsafe { GetWindowThreadProcessId(h, Some(&mut p)) };
                v.push((p, h));
                BOOL(1)
            }
            let mut all: Vec<(u32, HWND)> = Vec::new();
            unsafe {
                let _ = EnumWindows(Some(cb), LPARAM(&mut all as *mut _ as isize));
            }
            for (p, h) in all {
                if p != pid {
                    continue;
                }
                let mut b = [0u16; 128];
                let n = unsafe { GetClassNameW(h, &mut b) };
                let mut r = RECT::default();
                unsafe {
                    let _ = GetWindowRect(h, &mut r);
                }
                println!(
                    "  {:#x} class={} vis={} ({},{},{},{})",
                    h.0 as usize,
                    String::from_utf16_lossy(&b[..n as usize]),
                    unsafe { IsWindowVisible(h).as_bool() },
                    r.left, r.top, r.right, r.bottom
                );
            }
        }
        Some("dump") => {
            let pid: u32 = a[2].parse().unwrap();
            for (h, r) in windows(pid) {
                println!("  pid={pid} hwnd={:#x} ({},{},{},{})", h.0 as usize, r.left, r.top, r.right, r.bottom);
            }
        }
        Some("poke") => {
            let h = HWND(usize::from_str_radix(&a[2], 16).unwrap() as *mut core::ffi::c_void);
            let (x, y): (i32, i32) = (a[3].parse().unwrap(), a[4].parse().unwrap());
            unsafe {
                let _ = SetWindowPos(h, HWND::default(), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                let mut r = RECT::default();
                let _ = GetWindowRect(h, &mut r);
                println!("poke 后 @({},{})", r.left, r.top);
            }
        }
        Some("check") => {
            let pid: u32 = a[2].parse().unwrap();
            let cap = RECT {
                left: a[3].parse().unwrap(),
                top: a[4].parse().unwrap(),
                right: a[5].parse().unwrap(),
                bottom: a[6].parse().unwrap(),
            };
            flyout::watch(pid, cap, "测试图标");
            std::thread::sleep(std::time::Duration::from_millis(1200));
            for (h, r) in windows(pid) {
                println!("  pid={pid} hwnd={:#x} ({},{},{},{})", h.0 as usize, r.left, r.top, r.right, r.bottom);
            }
        }
        _ => eprintln!("用法: flyoutcheck popup <x> <y> <w> <h> | check <pid> <l> <t> <r> <b>"),
    }
}

fn windows(pid: u32) -> Vec<(HWND, RECT)> {
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let v = unsafe { &mut *(l.0 as *mut Vec<(u32, HWND, RECT)>) };
        let mut p = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut p)) };
        if p == 0 {
            return BOOL(1);
        }
        let mut r = RECT::default();
        unsafe {
            if GetWindowRect(h, &mut r).is_ok() && r.right > r.left {
                v.push((p, h, r));
            }
        }
        BOOL(1)
    }
    let mut all: Vec<(u32, HWND, RECT)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut all as *mut _ as isize));
    }
    all.into_iter()
        .filter(|(p, _, _)| *p == pid)
        .map(|(_, h, r)| (h, r))
        .collect()
}
