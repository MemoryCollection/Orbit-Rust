//! 系统级功能：开机启动、退出守护、电源动作（关机/重启/锁屏）。
//!
//! - 开机启动：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 写一个 `Orbit`
//!   值（REG_SZ = exe 全路径）。设置页的开关走 `apply_autostart`，启动时
//!   `sync_autostart` 按设置对齐注册表（手改 settings.txt 也能生效）。
//! - 退出守护：主进程隐藏任务栏成功后拉起一个守护子进程
//!   `orbit.exe --guardian=<父进程pid>`。子进程等父进程退出：父进程**正常退出**
//!   会先恢复任务栏并留下 `data/orbit.exit` 标记（`mark_clean_exit`），子进程
//!   看到新鲜标记就静默退出；否则说明 orbit 是被强杀的（任务栏停在隐藏态），
//!   执行 `force_restore` + `taskkill /f /im explorer.exe && start explorer.exe`
//!   把系统任务栏整个拉回来。
//! - 电源动作：关机/重启走 `ExitWindowsEx`（先启用 SeShutdownPrivilege），
//!   失败回落 `shutdown.exe`；锁屏走 `LockWorkStation`。

use std::os::windows::process::CommandExt;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_SZ,
};
use windows::Win32::System::Shutdown::{ExitWindowsEx, LockWorkStation};
use windows::Win32::System::Threading::OpenProcessToken;

/// 守护标记文件：父进程正常退出（已恢复任务栏）前写的一个新鲜时间戳
const MARKER: &str = "orbit.exit";
/// 标记在多久内算"新鲜"：超过就认为父进程不是正常退出
const MARKER_FRESH_MS: u64 = 30_000;
/// 后台进程创建标志（不弹控制台窗口）
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ---------- 单实例互斥 ----------

/// 尝试成为唯一的 orbit 实例。已有实例在跑时返回 `Err(())`。
/// 互斥体句柄故意不关闭：内核对象随进程存活，进程退出（含强杀）自动释放，
/// 不会出现"上次异常退出导致 mutex 卡死"的问题——这正是选命名互斥体
/// 而不是标记文件的原因（标记文件有残留风险）。
/// 互斥体名写在下面 `w!` 里（宏只收字面量）。
pub fn acquire_single_instance() -> Result<(), ()> {
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        // 创建失败（理论上几乎不会）也不拦着启动，只按 ALREADY_EXISTS 判重
        let h = CreateMutexW(None, true, w!("Orbit.SingleInstance.Mutex"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Err(());
        }
        // 句柄不 CloseHandle：握在手里直到进程退出
        core::mem::forget(h);
    }
    Ok(())
}

// ---------- 开机启动 ----------

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/// 把开机启动注册表项对齐到设置的值：开 = 写入，关 = 删除
pub fn apply_autostart(on: bool) {
    unsafe {
        let mut hk = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            windows::core::PCWSTR(run_key_wide().as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hk,
        )
        .is_err()
        {
            crate::tlog!("[guard] 打开 Run 键失败，开机启动未变更");
            return;
        }
        if on {
            let Some(exe) = std::env::current_exe().ok().map(|p| {
                format!("\"{}\"", p.display())
            }) else {
                let _ = RegCloseKey(hk);
                return;
            };
            let mut v: Vec<u16> = exe.encode_utf16().collect();
            v.push(0);
            let bytes: Vec<u8> = v.iter().flat_map(|c| c.to_le_bytes()).collect();
            let rc = RegSetValueExW(hk, w!("Orbit"), 0, REG_SZ, Some(&bytes));
            crate::tlog!("[guard] 开机启动 {}：{exe}（rc={rc:?}）", if on { "开启" } else { "关闭" });
        } else {
            let rc = RegDeleteValueW(hk, w!("Orbit"));
            crate::tlog!("[guard] 开机启动关闭（rc={rc:?}）");
        }
        let _ = RegCloseKey(hk);
    }
}

/// 启动时调用：注册表按 settings 的 autostart 值对齐（手改文件也生效）
pub fn sync_autostart() {
    apply_autostart(crate::settings::autostart());
}

fn run_key_wide() -> Vec<u16> {
    RUN_KEY.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------- 退出守护 ----------

/// 主进程启动时调用：拉起守护子进程（默认开；在设置里关掉后不再拉起）
pub fn spawn_guardian() {
    if !crate::settings::guardian() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let pid = std::process::id();
    std::thread::Builder::new()
        .name("guard-spawn".into())
        .spawn(move || {
            match std::process::Command::new(&exe)
                .arg(format!("--guardian={pid}"))
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
            {
                Ok(_) => crate::tlog!("[guard] 守护子进程已启动（父 pid={pid}）"),
                Err(e) => crate::tlog!("[guard] 守护子进程启动失败：{e}"),
            }
        })
        .ok();
}

/// 守护子进程入口：等父进程退出，异常退出时恢复任务栏并重启 explorer。
/// 此进程不建任何窗口，跑完即退。
pub fn run_guardian(parent: u32) {
    unsafe {
        use windows::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, INFINITE, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        };
        crate::tlog!("[guard] 守护子进程就绪，盯住父进程 pid={parent}");
        let Ok(h) = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            parent,
        ) else {
            crate::tlog!("[guard] 打不开父进程句柄，守护退出");
            return;
        };
        let wait = WaitForSingleObject(h, INFINITE);
        let _ = CloseHandle(h);
        if wait != windows::Win32::Foundation::WAIT_OBJECT_0 {
            crate::tlog!("[guard] 等待父进程失败，守护退出");
            return;
        }
        // 父进程正常退出前会写新鲜标记（任务栏已由父进程自己恢复），不用管
        if marker_fresh() {
            crate::tlog!("[guard] 父进程正常退出（任务栏已恢复），守护退出");
            return;
        }
        crate::tlog!("[guard] 父进程异常退出：恢复任务栏并重启 explorer");
        // 先按"无记录"兜底恢复 ABM 状态/可见性/工作区（父进程已死，它的恢复不会再来）
        crate::hide::force_restore();
        std::thread::sleep(Duration::from_millis(800));
        // 再把 explorer 整个重启一遍，任务栏以全新状态回来（用户指定的命令）
        let rc = std::process::Command::new("cmd")
            .args(["/c", "taskkill /f /im explorer.exe && start explorer.exe"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        crate::tlog!("[guard] explorer 重启命令 rc={rc:?}");
    }
}

/// 主进程正常退出（任务栏已 restore_all）时留新鲜标记，让守护子进程静默收场
pub fn mark_clean_exit() {
    let p = crate::store::path(MARKER);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_default();
    let _ = std::fs::write(p, now);
}

/// 标记是否存在且足够新鲜
fn marker_fresh() -> bool {
    let p = crate::store::path(MARKER);
    let Ok(m) = std::fs::metadata(&p) else { return false };
    let Ok(modified) = m.modified() else { return false };
    let Ok(age) = modified.elapsed() else { return false };
    age.as_millis() as u64 <= MARKER_FRESH_MS
}

// ---------- 电源动作 ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PowerAction {
    Shutdown,
    Restart,
    Lock,
}

pub fn power(action: PowerAction) {
    match action {
        PowerAction::Lock => unsafe {
            let _ = LockWorkStation();
        },
        PowerAction::Shutdown | PowerAction::Restart => unsafe {
            enable_shutdown_privilege();
            use windows::Win32::System::Shutdown::{
                EWX_FORCEIFHUNG, EWX_POWEROFF, EWX_REBOOT, EWX_SHUTDOWN,
            };
            // 不带 EWX_FORCE：给应用保存数据的机会；卡死的进程由 FORCEIFHUNG 处理
            let flags = if action == PowerAction::Restart {
                EWX_REBOOT | EWX_FORCEIFHUNG
            } else {
                EWX_SHUTDOWN | EWX_POWEROFF | EWX_FORCEIFHUNG
            };
            let ok = ExitWindowsEx(flags, windows::Win32::System::Shutdown::SHUTDOWN_REASON(0)).is_ok();
            if !ok {
                crate::tlog!("[guard] ExitWindowsEx 失败，回落 shutdown.exe");
                // 兜底：shutdown.exe 自己会提权
                let args: Vec<u16> = if action == PowerAction::Restart {
                    "/r /t 0\0".encode_utf16().collect()
                } else {
                    "/s /t 0\0".encode_utf16().collect()
                };
                let _ = windows::Win32::UI::Shell::ShellExecuteW(
                    HWND::default(),
                    w!("open"),
                    w!("shutdown.exe"),
                    PCWSTR(args.as_ptr()),
                    PCWSTR::null(),
                    windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
                );
            }
        },
    }
}

/// 启用 SeShutdownPrivilege（ExitWindowsEx 需要它）
unsafe fn enable_shutdown_privilege() {
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut token = HANDLE::default();
    if OpenProcessToken(
        GetCurrentProcess(),
        TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
        &mut token,
    )
    .is_err()
    {
        return;
    }
    let mut luid = Default::default();
    if LookupPrivilegeValueW(None, w!("SeShutdownPrivilege"), &mut luid).is_err() {
        let _ = CloseHandle(token);
        return;
    }
    let tp = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    let _ = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
    let _ = CloseHandle(token);
}
