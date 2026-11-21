//! 诊断 ITaskbarList::HrInit 为什么失败。
//!
//! 输出：
//!   - CoCreateInstance / HrInit 的 HRESULT
//!   - 当前 Shell_TrayWnd 的 HWND / PID / 可见性
//!   - 对 Shell_TrayWnd 发 TWM_GETTASKSWITCH (WM_USER+236) 的返回值
//!
//! 这个内部消息是 ITaskbarList::TaskWnd() 用来拿"任务切换窗口"的；
//! HiBitUninstaller 报 E_NOTIMPL 通常就是它返回 0。

use std::ffi::c_void;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HANDLE, HWND, LPARAM, RECT, TRUE, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumPropsExW, FindWindowExW, FindWindowW, GetClassNameW, GetShellWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowThreadProcessId, HWND_BOTTOM, HWND_TOPMOST, IsWindow, IsWindowVisible,
    SendMessageTimeoutW, SendMessageW, SetPropW, SetWindowPos, ShowWindow, SMTO_ABORTIFHUNG,
    SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, GWL_EXSTYLE, GWL_STYLE,
};

/// ITaskbarList::TaskWnd() 发给 Shell_TrayWnd 的内部消息。
const TWM_GETTASKSWITCH: u32 = 0x0400 + 236;

unsafe fn lower_foreign_trays() {
    let me = std::process::id();
    let mut cur = HWND::default();
    loop {
        let Ok(h) = FindWindowExW(HWND::default(), cur, w!("Shell_TrayWnd"), PCWSTR::null()) else {
            break;
        };
        cur = h;
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == 0 || pid == me {
            continue;
        }
        let _ = SetWindowPos(
            h, HWND_BOTTOM, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        println!("lower fake tray {:#x} pid={pid}", h.0 as usize);
    }
}

unsafe fn place_foreign_trays_like_real() {
    let me = std::process::id();
    let Ok(real) = FindWindowW(w!("Shell_TrayWnd"), w!("")) else { return };
    let mut real_pid = 0u32;
    let _ = GetWindowThreadProcessId(real, Some(&mut real_pid));
    if real_pid == me {
        println!("front 已是自己，跳过 place");
        return;
    }
    let mut rc = RECT::default();
    let _ = GetWindowRect(real, &mut rc);
    println!("real rect=({},{},{},{})", rc.left, rc.top, rc.right, rc.bottom);
    let mut cur = HWND::default();
    loop {
        let Ok(h) = FindWindowExW(HWND::default(), cur, w!("Shell_TrayWnd"), PCWSTR::null()) else {
            break;
        };
        cur = h;
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == 0 || pid == me || pid == real_pid {
            continue;
        }
        let _ = SetWindowPos(
            h, HWND_TOPMOST, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top,
            SWP_NOACTIVATE,
        );
        println!("place fake {:#x} pid={pid} to real rect", h.0 as usize);
    }
}

unsafe fn show_foreign_trays() {
    let me = std::process::id();
    let mut cur = HWND::default();
    loop {
        let Ok(h) = FindWindowExW(HWND::default(), cur, w!("Shell_TrayWnd"), PCWSTR::null()) else {
            break;
        };
        cur = h;
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == 0 || pid == me {
            continue;
        }
        let _ = ShowWindow(h, SW_SHOW);
        println!("show fake tray {:#x} pid={pid}", h.0 as usize);
    }
}

unsafe fn hide_foreign_trays() {
    let me = std::process::id();
    let mut cur = HWND::default();
    loop {
        let Ok(h) = FindWindowExW(HWND::default(), cur, w!("Shell_TrayWnd"), PCWSTR::null()) else {
            break;
        };
        cur = h;
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == 0 || pid == me {
            continue;
        }
        let _ = ShowWindow(h, SW_HIDE);
        println!("hide fake tray {:#x} pid={pid}", h.0 as usize);
    }
}

unsafe extern "system" fn prop_cb(
    _h: HWND,
    name: windows::core::PCWSTR,
    _value: HANDLE,
    lparam: usize,
) -> BOOL {
    let out = &mut *(lparam as *mut Vec<String>);
    if !name.0.is_null() {
        if name.0 as usize <= 0xFFFF {
            out.push(format!("#{}", name.0 as usize));
        } else {
            let mut v = Vec::new();
            let mut p = name.0;
            while *p != 0 {
                v.push(*p);
                p = p.add(1);
            }
            out.push(String::from_utf16_lossy(&v));
        }
    }
    TRUE
}

fn props_of(h: HWND) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(64);
    unsafe {
        let _ = EnumPropsExW(
            h,
            Some(prop_cb),
            LPARAM(&mut out as *mut Vec<String> as isize),
        );
    }
    out
}

unsafe fn set_taskband_prop_on_fakes() {
    let me = std::process::id();
    let shell = GetShellWindow();
    let mut shell_pid = 0u32;
    let _ = GetWindowThreadProcessId(shell, Some(&mut shell_pid));
    println!("shell hwnd={:#x} pid={shell_pid}", shell.0 as usize);
    let mut cur = HWND::default();
    loop {
        let Ok(h) = FindWindowExW(HWND::default(), cur, w!("Shell_TrayWnd"), PCWSTR::null()) else {
            break;
        };
        cur = h;
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == 0 || pid == me || pid == shell_pid {
            continue;
        }
        let _ = SetPropW(h, w!("TaskbandHWND"), HANDLE(shell.0));
        println!("set TaskbandHWND on fake {:#x} → {:#x}", h.0 as usize, shell.0 as usize);
    }
}

fn main() {
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        println!("CoInitializeEx = 0x{:08X}", init.0);

        let mode = std::env::args().nth(1).unwrap_or_else(|| "normal".to_string());
        if mode == "lower" {
            // 诊断用：把非本进程的假 Shell_TrayWnd 压到底层，让 FindWindow 命中真 Explorer。
            // trayhost 的 guard_loop 100ms 后会抬回来，所以这里压完立刻测 HrInit。
            lower_foreign_trays();
        } else if mode == "hide" {
            hide_foreign_trays();
        } else if mode == "show" {
            show_foreign_trays();
        } else if mode == "place" {
            place_foreign_trays_like_real();
        } else if mode == "setprop" {
            set_taskband_prop_on_fakes();
        }
        if let Ok(front) = FindWindowW(w!("Shell_TrayWnd"), w!("")) {
            let mut pid = 0u32;
            let _ = GetWindowThreadProcessId(front, Some(&mut pid));
            println!("FindWindow front={:#x} pid={} mode={mode}", front.0 as usize, pid);
        }

        match CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_INPROC_SERVER) {
            Ok(tb) => match tb.HrInit() {
                Ok(()) => println!("ITaskbarList::HrInit = S_OK"),
                Err(e) => {
                    let hr = e.code().0 as u32;
                    println!(
                        "ITaskbarList::HrInit = 0x{hr:08X} ({})",
                        hr as i32
                    );
                }
            },
            Err(e) => println!("CoCreateInstance 失败: {:?} (0x{:08X})", e, e.code().0 as u32),
        }

        println!("\n== Shell_TrayWnd 枚举 ==");
        let me = std::process::id();
        let mut cur = HWND::default();
        loop {
            let Ok(h) = FindWindowExW(HWND::default(), cur, w!("Shell_TrayWnd"), PCWSTR::null()) else {
                break;
            };
            cur = h;
            let mut pid = 0u32;
            let _ = GetWindowThreadProcessId(h, Some(&mut pid));
            let mut cls = [0u16; 64];
            let n = GetClassNameW(h, &mut cls);
            let class = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
            let visible = IsWindowVisible(h).as_bool();
            let style = GetWindowLongPtrW(h, GWL_STYLE);
            let exstyle = GetWindowLongPtrW(h, GWL_EXSTYLE);
            let props = props_of(h);
            let mut result: usize = 0;
            let ok = SendMessageTimeoutW(
                h,
                TWM_GETTASKSWITCH,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                1000,
                Some(&mut result),
            );
            let task = HWND(result as *mut c_void);
            let task_valid = if result != 0 && IsWindow(task).as_bool() { "valid" } else { "INVALID/0" };
            let plain = SendMessageW(h, TWM_GETTASKSWITCH, WPARAM(0), LPARAM(0)).0 as usize;
            let plain_task = HWND(plain as *mut c_void);
            let plain_valid = if plain != 0 && IsWindow(plain_task).as_bool() { "valid" } else { "INVALID/0" };
            println!(
                "hwnd={:#x} pid={} (self={}) class={:?} visible={} style={:#x} ex={:#x} props={:?} TWM_GETTASKSWITCH ok={} → {:#x} [{task_valid}]  plain → {:#x} [{plain_valid}]",
                h.0 as usize, pid, pid == me, class, visible, style as usize, exstyle as usize, props, ok.0 != 0, result, plain
            );
        }
    }
}
