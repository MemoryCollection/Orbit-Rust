//! 试验任务栏隐藏方式对"注入悬停是否弹卡片"的影响。
//! auto = 只设自动隐藏；hide = 自动隐藏 + SWP_HIDEWINDOW；show = 还原。

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, APPBARDATA, ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetWindowRect, IsWindowVisible, SetWindowPos, ShowWindow,
    SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOW,
};

fn tray() -> Option<HWND> {
    unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).ok() }
}

fn abd(h: HWND) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: h,
        ..Default::default()
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mode = a.get(1).map(|s| s.as_str()).unwrap_or("state");
    let Some(h) = tray() else {
        println!("没找到 Shell_TrayWnd");
        return;
    };
    unsafe {
        match mode {
            "auto" => {
                let mut d = abd(h);
                d.lParam = LPARAM(ABS_AUTOHIDE as isize);
                println!("ABM_SETSTATE 返回 {}", SHAppBarMessage(ABM_SETSTATE, &mut d));
            }
            "hide" => {
                let mut d = abd(h);
                d.lParam = LPARAM(ABS_AUTOHIDE as isize);
                SHAppBarMessage(ABM_SETSTATE, &mut d);
                let _ = SetWindowPos(
                    h,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
                println!("已 auto-hide + SWP_HIDEWINDOW");
            }
            "alpha" => {
                // alpha <0-255>
                let a: u8 = a.get(2).map(|s| s.parse().unwrap()).unwrap_or(1);
                use windows::Win32::UI::WindowsAndMessaging::{
                    SetLayeredWindowAttributes, GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW,
                };
                let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
                SetWindowLongPtrW(h, GWL_EXSTYLE, ex | 0x80000);
                let _ = SetLayeredWindowAttributes(
                    h,
                    windows::Win32::Foundation::COLORREF(0),
                    a,
                    windows::Win32::UI::WindowsAndMessaging::LWA_ALPHA,
                );
                println!("alpha={a}");
            }
            "unlayer" => {
                use windows::Win32::UI::WindowsAndMessaging::{
                    GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE,
                };
                let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
                SetWindowLongPtrW(h, GWL_EXSTYLE, ex & !0x80000);
                println!("已去掉 WS_EX_LAYERED (ex 0x{ex:x} → 0x{:x})", ex & !0x80000);
            }
            "wa" => {
                // 只改工作区（不动任务栏）：让系统认为底部没有占位
                let mut wa = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
                let ok = windows::Win32::UI::WindowsAndMessaging::SystemParametersInfoW(
                    windows::Win32::UI::WindowsAndMessaging::SPI_SETWORKAREA,
                    0,
                    Some(&mut wa as *mut _ as *mut _),
                    Default::default(),
                );
                println!("SPI_SETWORKAREA -> {:?}", ok);
            }
            "offscreen" => {
                let mut d = abd(h);
                d.lParam = LPARAM(ABS_AUTOHIDE as isize);
                SHAppBarMessage(ABM_SETSTATE, &mut d);
                let _ = SetWindowPos(h, None, 0, 1080, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                println!("已移到屏幕外 (0,1080)，保持 visible");
            }
            "back" => {
                let _ = SetWindowPos(h, None, 0, 1032, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                let mut d = abd(h);
                d.lParam = LPARAM(0);
                SHAppBarMessage(ABM_SETSTATE, &mut d);
                println!("已移回 (0,1032)");
            }
            "show" => {
                let mut d = abd(h);
                d.lParam = LPARAM(0);
                SHAppBarMessage(ABM_SETSTATE, &mut d);
                let _ = ShowWindow(h, SW_SHOW);
                println!("已还原");
            }
            _ => {}
        }
        let mut d = abd(h);
        let state = SHAppBarMessage(ABM_GETSTATE, &mut d);
        let mut r = RECT::default();
        let _ = GetWindowRect(h, &mut r);
        // 工作区：看任务栏还占不占位
        let mut wa = RECT::default();
        windows::Win32::UI::WindowsAndMessaging::SystemParametersInfoW(
            windows::Win32::UI::WindowsAndMessaging::SPI_GETWORKAREA,
            0,
            Some(&mut wa as *mut _ as *mut _),
            Default::default(),
        )
        .ok();
        println!(
            "state=0x{state:x} visible={} rect=({},{},{},{}) workarea=({},{},{},{})",
            IsWindowVisible(h).as_bool(),
            r.left, r.top, r.right, r.bottom,
            wa.left, wa.top, wa.right, wa.bottom
        );
        // 顺带看看 TrayNotifyWnd 还在不在
        if let Ok(n) = FindWindowExW(h, None, w!("TrayNotifyWnd"), PCWSTR::null()) {
            println!("TrayNotifyWnd ok {:?}", n.0 as usize);
        }
    }
}
