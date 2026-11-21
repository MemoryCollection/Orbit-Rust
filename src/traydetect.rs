//! 第三方托盘宿主检测（对齐 Atrium：「检测到 RetroBar、ExplorerPatcher 等
//! 第三方托盘宿主已经存在，就不争抢，而是回到读取模式」）。
//!
//! 两路证据，任一命中就认为托盘已被别的程序接管：
//!   1. 进程名：常见第三方任务栏/托盘宿主。
//!   2. 任务栏窗口属主：`Shell_TrayWnd` 不属于 explorer.exe 也不属于自己，
//!      说明有未知宿主已经换掉了系统任务栏。
//!
//! 命中后调用方（`app.rs`）不建假托盘、不隐藏真任务栏，退回 UIA 只读镜像。

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowThreadProcessId};

/// 已知第三方托盘/任务栏宿主进程名（小写）
const KNOWN_HOSTS: [&str; 6] = [
    "retrobar.exe",
    "explorerpatcher.exe",
    "startallback.exe",
    "startisback.exe",
    "openshell.exe",
    "classicshell.exe",
];

fn base_name(raw: &[u16]) -> String {
    let text = String::from_utf16_lossy(raw);
    text.trim_end_matches('\0')
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .to_lowercase()
}

/// pid → exe 文件基名（小写），取不到为空
fn exe_base_of(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let Ok(proc) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(proc, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
            .is_ok();
        let _ = CloseHandle(proc);
        if ok && len > 0 {
            base_name(&buf[..len as usize])
        } else {
            String::new()
        }
    }
}

/// 检测到第三方托盘宿主时返回其进程名（小写），否则 None。
pub fn third_party_host() -> Option<String> {
    unsafe {
        // 1) 进程名扫描
        if let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            let mut entry = PROCESSENTRY32W::default();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut entry).is_ok();
            while ok {
                let name = base_name(&entry.szExeFile);
                if KNOWN_HOSTS.contains(&name.as_str()) {
                    let _ = CloseHandle(snap);
                    crate::tlog!("[traydetect] 检测到第三方托盘宿主进程 {name}，不争抢，退回只读");
                    return Some(name);
                }
                ok = Process32NextW(snap, &mut entry).is_ok();
            }
            let _ = CloseHandle(snap);
        }

        // 2) 任务栏窗口属主：不是 explorer.exe 也不是自己 → 未知宿主已接管
        let me = std::process::id();
        let mut cur = HWND::default();
        loop {
            let Ok(n) = FindWindowExW(HWND::default(), cur, w!("Shell_TrayWnd"), PCWSTR::null()) else {
                break;
            };
            cur = n;
            let mut pid = 0u32;
            let _ = GetWindowThreadProcessId(n, Some(&mut pid));
            if pid == 0 || pid == me {
                continue;
            }
            let name = exe_base_of(pid);
            if !name.is_empty() && name != "explorer.exe" {
                crate::tlog!(
                    "[traydetect] Shell_TrayWnd 属主是 {name}（非 explorer），判定已有第三方宿主，退回只读"
                );
                return Some(name);
            }
        }
    }
    None
}
