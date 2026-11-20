//! 直接复用产品里的 `src/traymsg.rs` 跑一遍"找回调目标 + 转发"，用来验证真代码。

#[macro_export]
macro_rules! vlog {
    ($($t:tt)*) => { println!($($t)*) };
}

#[path = "../../../src/traymsg.rs"]
mod traymsg;

use windows::Win32::Foundation::RECT;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.get(1).map(|s| s.as_str()) == Some("find") {
        for arg in &a[2..] {
            let pid: u32 = arg.parse().unwrap();
            for (h, uid, r, class) in traymsg::find_icons(pid) {
                println!(
                    "pid={pid} hwnd={:#x} uid={uid} class={class} rect=({},{},{},{})",
                    h.0 as usize, r.left, r.top, r.right, r.bottom
                );
            }
        }
        return;
    }
    if a.get(1).map(|s| s.as_str()) == Some("winpid") {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::*;
        let hwnd = usize::from_str_radix(a[2].trim_start_matches("0x"), 16).unwrap();
        let h = HWND(hwnd as *mut core::ffi::c_void);
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        let mut b = [0u16; 256];
        let n = unsafe { GetClassNameW(h, &mut b) };
        let mut r = windows::Win32::Foundation::RECT::default();
        unsafe {
            let _ = GetWindowRect(h, &mut r);
        }
        let t = traymsg::Target { pid, hwnd: h, msg: 0, uid: 0, ver: 4 };
        println!(
            "pid={pid} class={:?} rect=({},{},{},{}) 图标中心={:?}",
            String::from_utf16_lossy(&b[..n as usize]),
            r.left, r.top, r.right, r.bottom,
            traymsg::icon_center_of(t)
        );
        return;
    }
    if a.get(1).map(|s| s.as_str()) == Some("realhover") {
        // realhover <hwnd_hex> <uid> <pid...>：真把光标放上去，看谁变了
        use windows::Win32::Foundation::{HWND, POINT};
        use windows::Win32::UI::WindowsAndMessaging::*;
        let hwnd = usize::from_str_radix(a[2].trim_start_matches("0x"), 16).unwrap();
        let uid: u32 = a[3].parse().unwrap();
        let pids: Vec<u32> = a[4..].iter().map(|s| s.parse().unwrap()).collect();
        let t = traymsg::Target {
            pid: 0,
            hwnd: HWND(hwnd as *mut core::ffi::c_void),
            msg: 0,
            uid,
            ver: 4,
        };
        let c = traymsg::icon_center_of(t).expect("查不到矩形");
        let mut snaps: Vec<std::collections::HashSet<String>> = pids.iter().map(|&p| windows_of(p)).collect();
        let mut old = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut old);
            let _ = SetCursorPos(c.0, c.1);
        }
        std::thread::sleep(std::time::Duration::from_millis(1500));
        unsafe {
            let _ = SetCursorPos(old.x, old.y);
        }
        for (i, &p) in pids.iter().enumerate() {
            let now = windows_of(p);
            for w in now.difference(&snaps[i]) {
                println!("  pid={p} 新窗口 {w}");
            }
            snaps[i] = now;
        }
        println!("真实悬停 @{c:?} 结束");
        return;
    }
    if a.get(1).map(|s| s.as_str()) == Some("seqat") {
        // seqat <hwnd_hex> <uid> <msg_hex> <ver> <事件列表> <x> <y>
        let hwnd = usize::from_str_radix(a[2].trim_start_matches("0x"), 16).unwrap();
        let uid: u32 = a[3].parse().unwrap();
        let msg = u32::from_str_radix(a[4].trim_start_matches("0x"), 16).unwrap();
        let ver: u32 = a[5].parse().unwrap();
        let (x, y): (i32, i32) = (a[7].parse().unwrap(), a[8].parse().unwrap());
        let before = all_windows();
        for ev in a[6].split(',') {
            let ev = u32::from_str_radix(ev.trim().trim_start_matches("0x"), 16).unwrap();
            let (w, l) = traymsg::encode_pub(ver, uid, ev, x, y);
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::SendNotifyMessageW(
                    windows::Win32::Foundation::HWND(hwnd as *mut core::ffi::c_void),
                    msg,
                    windows::Win32::Foundation::WPARAM(w as usize),
                    windows::Win32::Foundation::LPARAM(l as isize),
                );
            }
            println!("  发 {ev:#x} w={w:#x} l={l:#x}");
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let after = all_windows();
        for w in after.difference(&before) {
            println!("  新窗口 {w}");
        }
        if after.difference(&before).next().is_none() {
            println!("  没有新窗口");
        }
        return;
    }
    if a.get(1).map(|s| s.as_str()) == Some("seq") {
        // seq <hwnd_hex> <uid> <msg_hex> <ver> <事件逗号列表> [sync]
        use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
        let hwnd = usize::from_str_radix(a[2].trim_start_matches("0x"), 16).unwrap();
        let uid: u32 = a[3].parse().unwrap();
        let msg = u32::from_str_radix(a[4].trim_start_matches("0x"), 16).unwrap();
        let ver: u32 = a[5].parse().unwrap();
        let sync = a.get(7).map(|s| s == "sync").unwrap_or(false);
        let t = traymsg::Target {
            pid: 0,
            hwnd: windows::Win32::Foundation::HWND(hwnd as *mut core::ffi::c_void),
            msg,
            uid,
            ver,
        };
        let c = traymsg::icon_center_of(t).expect("查不到图标矩形");
        let before = all_windows();
        for ev in a[6].split(',') {
            let ev = u32::from_str_radix(ev.trim().trim_start_matches("0x"), 16).unwrap();
            let (w, l) = traymsg::encode_pub(ver, uid, ev, c.0, c.1);
            unsafe {
                if sync {
                    SendMessageW(t.hwnd, t.msg, windows::Win32::Foundation::WPARAM(w as usize), windows::Win32::Foundation::LPARAM(l as isize));
                } else {
                    let _ = windows::Win32::UI::WindowsAndMessaging::SendNotifyMessageW(
                        t.hwnd, t.msg, windows::Win32::Foundation::WPARAM(w as usize), windows::Win32::Foundation::LPARAM(l as isize));
                }
            }
            println!("  发 {ev:#x} w={w:#x} l={l:#x} sync={sync}");
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let after = all_windows();
        for w in after.difference(&before) {
            println!("  新窗口 {w}");
        }
        if after.difference(&before).next().is_none() {
            println!("  没有新窗口");
        }
        return;
    }
    if a.get(1).map(|s| s.as_str()) == Some("hotsim") {
        // hotsim <hwnd_hex> <uid> <msg_hex> <ver> <pid>
        let hwnd = usize::from_str_radix(a[2].trim_start_matches("0x"), 16).unwrap();
        let uid: u32 = a[3].parse().unwrap();
        let msg = u32::from_str_radix(a[4].trim_start_matches("0x"), 16).unwrap();
        let ver: u32 = a[5].parse().unwrap();
        let t = traymsg::Target {
            pid: 0,
            hwnd: windows::Win32::Foundation::HWND(hwnd as *mut core::ffi::c_void),
            msg,
            uid,
            ver,
        };
        let c = traymsg::icon_center_of(t).expect("查不到图标矩形");
        let before = all_windows();
        // 先像 explorer 那样连续发 mousemove 把应用的 hover 跟踪"喂"起来
        for _ in 0..3 {
            traymsg::hover_move(&t, c);
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        traymsg::hover(&t, c);
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let after = all_windows();
        for w in after.difference(&before) {
            println!("  新窗口 {w}");
        }
        if after.difference(&before).next().is_none() {
            println!("  没有新窗口");
        }
        return;
    }
    if a.get(1).map(|s| s.as_str()) == Some("hover") {
        // hover <hwnd_hex> <uid> <msg_hex> <ver> <pid>
        let hwnd = usize::from_str_radix(a[2].trim_start_matches("0x"), 16).unwrap();
        let uid: u32 = a[3].parse().unwrap();
        let msg = u32::from_str_radix(a[4].trim_start_matches("0x"), 16).unwrap();
        let ver: u32 = a[5].parse().unwrap();
        let pid: u32 = a[6].parse().unwrap();
        let t = traymsg::Target {
            pid,
            hwnd: windows::Win32::Foundation::HWND(hwnd as *mut core::ffi::c_void),
            msg,
            uid,
            ver,
        };
        let before = windows_of(pid);
        let c = traymsg::icon_center_of(t).expect("查不到图标矩形");
        println!("图标中心 {c:?}，发 WM_MOUSEHOVER + NIN_POPUPOPEN（v{ver}）");
        traymsg::hover(&t, c);
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let after = windows_of(pid);
        for w in after.difference(&before) {
            println!("  新窗口 {w}");
        }
        if after.difference(&before).next().is_none() {
            println!("  没有新窗口");
        }
        return;
    }
    if a.get(1).map(|s| s.as_str()) == Some("listwhich") {
        for e in traymsg::tray_items() {
            println!("{:#x} {} {:?}", e.hwnd.0 as usize, e.uid, e.tip);
        }
        return;
    }
    if a.get(1).map(|s| s.as_str()) == Some("auto") {
        use windows::core::{Interface, GUID, HRESULT};
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
        use windows::Win32::Foundation::BOOL;
        const CLSID: GUID = GUID::from_u128(0x25DEAD04_1EAC_4911_9E3A_AD0A4AB560FD);
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let obj: windows::core::Result<windows::core::IUnknown> = CoCreateInstance(&CLSID, None, CLSCTX_ALL);
            println!("CoCreateInstance ok={}", obj.is_ok());
            if let Ok(obj) = obj {
                let p = obj.as_raw();
                let vt = *(p as *mut *const usize);
                // vtable: 0..2 IUnknown, 3 RegisterCallback, 4 Unregister, 5 SetPreference, 6 EnableAutoTray
                let enable: extern "system" fn(*mut core::ffi::c_void, BOOL) -> HRESULT =
                    std::mem::transmute(*vt.add(6));
                let _ = enable(p, BOOL(0));
                std::thread::sleep(std::time::Duration::from_millis(600));
                println!("EnableAutoTray(false) 后：");
                for e in traymsg::tray_items() {
                    println!("  hwnd={:#x} uid={} msg={:#x} v{} tip={:?}", e.hwnd.0 as usize, e.uid, e.msg, e.ver, e.tip);
                }
                let _ = enable(p, BOOL(1));
                let _ = windows::core::Interface::as_raw(&obj);
                std::mem::forget(obj);
            }
        }
        return;
    }
    if a.get(1).map(|s| s.as_str()) == Some("dbg") {
        use windows::core::{w, PCWSTR};
        use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
        use windows::Win32::UI::WindowsAndMessaging::*;
        unsafe {
            let tray = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null());
            println!("Shell_TrayWnd = {:?}", tray.as_ref().map(|h| h.0 as usize));
            if let Ok(tray) = tray {
                let mut err = 0u32;
                let mut c = FindWindowExW(tray, None, PCWSTR::null(), PCWSTR::null());
                while let Ok(h) = c {
                    let mut b = [0u16; 128];
                    let n = GetClassNameW(h, &mut b);
                    println!("  child {:#x} {}", h.0 as usize, String::from_utf16_lossy(&b[..n as usize]));
                    c = FindWindowExW(tray, h, PCWSTR::null(), PCWSTR::null());
                    err += 1;
                    if err > 40 { break }
                }
                if let Ok(n) = FindWindowExW(tray, None, w!("TrayNotifyWnd"), PCWSTR::null()) {
                    let mut c = FindWindowExW(n, None, PCWSTR::null(), PCWSTR::null());
                    while let Ok(h) = c {
                        let mut b = [0u16; 128];
                        let k = GetClassNameW(h, &mut b);
                        println!("  notify child {:#x} {}", h.0 as usize, String::from_utf16_lossy(&b[..k as usize]));
                        c = FindWindowExW(n, h, PCWSTR::null(), PCWSTR::null());
                    }
                }
                // 直接看 TrayNotifyWnd 下的 ToolbarWindow32
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::*;
            let n = FindWindowExW(tray, None, w!("TrayNotifyWnd"), PCWSTR::null()).unwrap();
            let tb = FindWindowExW(n, None, w!("ToolbarWindow32"), PCWSTR::null());
            println!("direct ToolbarWindow32 = {:?}", tb.as_ref().map(|h| h.0 as usize));
            if let Ok(tb) = tb {
                println!("  BUTTONCOUNT = {}", SendMessageW(tb, 0x0418, None, None).0);
            }
        }
        // 所有 ToolbarWindow32
                let mut found: Vec<isize> = Vec::new();
                unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
                    let v = &mut *(l.0 as *mut Vec<isize>);
                    let mut b = [0u16; 128];
                    let n = GetClassNameW(h, &mut b);
                    if String::from_utf16_lossy(&b[..n as usize]) == "ToolbarWindow32" {
                        v.push(h.0 as isize);
                    }
                    BOOL(1)
                }
                let _ = EnumWindows(Some(cb), LPARAM(&mut found as *mut _ as isize));
                println!("ToolbarWindow32: {found:?}");
            }
        }
        return;
    }
    if a.get(1).map(|s| s.as_str()) == Some("list") {
        for e in traymsg::tray_items() {
            println!(
                "hwnd={:#x} uid={} msg={:#x} v{} guid={:?} tip={:?} exe={:?}",
                e.hwnd.0 as usize, e.uid, e.msg, e.ver, e.guid, e.tip, e.exe
            );
        }
        return;
    }
    if a.len() < 6 {
        eprintln!("用法: traymsgcheck <exe> <left> <top> <right> <bottom> [click|hover]");
        return;
    }
    let exe = a[1].to_lowercase();
    let icon = RECT {
        left: a[2].parse().unwrap(),
        top: a[3].parse().unwrap(),
        right: a[4].parse().unwrap(),
        bottom: a[5].parse().unwrap(),
    };
    let t0 = std::time::Instant::now();
    let t = traymsg::target(&exe, icon, 0);
    println!("耗时 {:?} 结果 {:?}", t0.elapsed(), t);
    let Some(t) = t else { return };
    let c = ((icon.left + icon.right) / 2, (icon.top + icon.bottom) / 2);
    match a.get(6).map(|s| s.as_str()) {
        Some("click") => {
            traymsg::click(&t, c, false);
            std::thread::sleep(std::time::Duration::from_millis(600));
            let fg = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            let mut b = [0u16; 256];
            let n = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(fg, &mut b) };
            println!("已投左键 @{c:?} → 前台 {:#x} {:?}", fg.0 as usize, String::from_utf16_lossy(&b[..n as usize]));
        }
        Some("hover") => {
            traymsg::hover(&t, c);
            println!("已投悬停 @{c:?}");
        }
        _ => {}
    }
}

fn windows_of(pid: u32) -> std::collections::HashSet<String> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let v = &mut *(l.0 as *mut Vec<HWND>);
        v.push(h);
        BOOL(1)
    }
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut all as *mut _ as isize));
        let _ = EnumChildWindows(HWND::default(), Some(cb), LPARAM(&mut all as *mut _ as isize));
    }
    let mut out = std::collections::HashSet::new();
    for h in all {
        let mut p = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut p)) };
        if p != pid {
            continue;
        }
        let mut b = [0u16; 256];
        let n = unsafe { GetClassNameW(h, &mut b) };
        let mut r = windows::Win32::Foundation::RECT::default();
        unsafe {
            let _ = GetWindowRect(h, &mut r);
        }
        out.insert(format!(
            "{} {:?} vis={} ({},{},{},{})",
            String::from_utf16_lossy(&b[..n as usize]),
            h.0 as usize,
            unsafe { IsWindowVisible(h).as_bool() },
            r.left, r.top, r.right, r.bottom
        ));
    }
    out
}

/// 全部可见顶层窗口（跨进程）
fn all_windows() -> std::collections::HashSet<String> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let v = unsafe { &mut *(l.0 as *mut Vec<String>) };
        let mut p = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut p)) };
        let mut b = [0u16; 128];
        let n = unsafe { GetClassNameW(h, &mut b) };
        let mut r = windows::Win32::Foundation::RECT::default();
        unsafe {
            if GetWindowRect(h, &mut r).is_ok() && IsWindowVisible(h).as_bool() {
                v.push(format!(
                    "{} {} ({},{},{},{}) pid={}",
                    String::from_utf16_lossy(&b[..n as usize]),
                    h.0 as usize, r.left, r.top, r.right, r.bottom, p
                ));
            }
        }
        BOOL(1)
    }
    let mut v: Vec<String> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v.into_iter().collect()
}
