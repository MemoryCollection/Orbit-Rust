//! 系统级功能：开机启动、退出守护（含复活）、电源动作（关机/重启/锁屏）。
//!
//! - 开机启动：双保险。主通道是计划任务 `Orbit`（`schtasks /sc onlogon`，登录后
//!   延迟 5s 启动）——HKCU Run 键会被火绒等安全软件的"启动项管理"在系统层面
//!   静默拦截（不改注册表、不留日志），计划任务不走那条管辖路径；Run 键继续
//!   写一份，两者谁先拉起都行（单实例互斥去重）。设置页的开关走 `apply_autostart`，
//!   启动时 `sync_autostart` 按设置对齐（手改 settings.txt 也能生效）。
//! - 退出守护：主进程隐藏任务栏成功后拉起一个守护子进程
//!   `orbit.exe --guardian=<父进程pid>`。子进程等父进程退出：
//!   - **正常退出**（Ctrl+Alt+O）：父进程先恢复任务栏并留下 `orbit.exit`
//!     `clean` 标记（`mark_clean_exit`），守护静默退出；
//!   - **panic 退出**：panic 钩子恢复任务栏后留 `panic` 标记（`mark_panic_exit`），
//!     守护不重启 explorer（任务栏已经回来了），但会复活 orbit；
//!   - **异常退出**（强杀等）：恢复任务栏 + 复活 orbit；复活次数用尽才重启
//!     explorer 兜底。
//!   - **复活**：重新拉起 orbit（最多连续 3 次）。复活出来的进程带
//!     `ORBIT_NO_GUARDIAN=1`，不再自己拉守护，由本守护进程继续盯。新 orbit
//!     稳定跑满 60s = 复活成功，复活计数清零（正常退出 / 全新启动同样清零）。
//! - 电源动作：关机/重启走 `ExitWindowsEx`（先启用 SeShutdownPrivilege），
//!   失败回落 `shutdown.exe`；锁屏走 `LockWorkStation`。

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_SZ,
};
use windows::Win32::System::Shutdown::{ExitWindowsEx, LockWorkStation};
use windows::Win32::System::Threading::OpenProcessToken;
use windows::Win32::UI::Shell::{
    IShellLinkW, ShellExecuteW, ShellLink, SHGetKnownFolderPath, FOLDERID_Startup,
    KNOWN_FOLDER_FLAG,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// 守护标记文件：orbit 退出前写的一行「类型 时间戳」。
/// clean = 正常退出（任务栏已恢复，含 Ctrl+Alt+O）；panic = panic 退出（钩子已恢复）；
/// 没有新鲜标记 = 异常退出（强杀等，任务栏停在隐藏态）。旧版本只写时间戳，按 clean 处理。
const MARKER: &str = "orbit.exit";
/// 标记在多久内算"新鲜"：超过就认为父进程不是正常退出
const MARKER_FRESH_MS: u64 = 30_000;
/// 复活计数文件：连续异常退出时复活 orbit 的次数（复活成功 / 正常退出 / 全新启动清零）
const REVIVE_FILE: &str = "orbit.revive";
/// 连续复活上限
const MAX_REVIVE: u32 = 3;
/// 复活出来的 orbit 稳定跑多久算「复活成功」（成功后计数清零）
const REVIVE_SUCCESS_MS: u64 = 60_000;
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
/// 计划任务名（三通道里最抗拦截的一条；根目录建任务需要管理员，开关时弹一次 UAC）
const TASK_NAME: &str = "Orbit";
/// 启动文件夹快捷方式文件名（第三条通道，纯文件操作免管理员）
const STARTUP_LNK: &str = "orbit.lnk";

/// 把开机启动对齐到设置的值。用户在设置页开关时调用（apply_autostart）：
/// 三条通道都动，计划任务失败（用户取消 UAC）不影响另两条。
pub fn apply_autostart(on: bool) {
    set_run_key(on);
    set_startup_lnk(on);
    if on {
        set_scheduled_task();
    } else {
        del_scheduled_task();
    }
}

/// 启动时按 settings 对齐：只动免管理员的两条通道（Run 键 + 启动文件夹快捷方式），
/// 不碰计划任务——开机/启动路径上绝不能弹 UAC。
pub fn sync_autostart() {
    let on = crate::settings::autostart();
    set_run_key(on);
    set_startup_lnk(on);
}

fn set_run_key(on: bool) {
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
        let rc;
        if on {
            let exe = std::env::current_exe().ok().map(|p| p.display().to_string());
            let Some(exe) = exe else {
                let _ = RegCloseKey(hk);
                return;
            };
            let mut v: Vec<u16> = format!("\"{exe}\"").encode_utf16().collect();
            v.push(0);
            let bytes: Vec<u8> = v.iter().flat_map(|c| c.to_le_bytes()).collect();
            rc = RegSetValueExW(hk, w!("Orbit"), 0, REG_SZ, Some(&bytes));
            crate::tlog!("[guard] Run 键开机启动开启：{exe}（rc={rc:?}）");
        } else {
            rc = RegDeleteValueW(hk, w!("Orbit"));
            crate::tlog!("[guard] Run 键开机启动关闭（rc={rc:?}）");
        }
        let _ = RegCloseKey(hk);
    }
}

/// 启动文件夹快捷方式（shell:startup）：免管理员，与 Run 键互为备份
fn set_startup_lnk(on: bool) {
    let Some(p) = startup_lnk_path() else {
        crate::tlog!("[guard] 拿不到启动文件夹路径，快捷方式通道跳过");
        return;
    };
    if !on {
        match std::fs::remove_file(&p) {
            Ok(_) => crate::tlog!("[guard] 启动文件夹快捷方式已删除"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => crate::tlog!("[guard] 启动文件夹快捷方式删除失败：{e}"),
        }
        return;
    }
    let Some(exe) = std::env::current_exe().ok() else { return };
    match write_lnk(&p, &exe) {
        Ok(_) => crate::tlog!("[guard] 启动文件夹快捷方式已写入：{}", p.display()),
        Err(e) => crate::tlog!("[guard] 启动文件夹快捷方式写入失败：{e}"),
    }
}

fn startup_lnk_path() -> Option<std::path::PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Startup, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let s = p.to_string().ok()?;
        let _ = CoTaskMemFree(Some(p.as_ptr().cast()));
        Some(std::path::Path::new(&s).join(STARTUP_LNK))
    }
}

/// 用 IShellLinkW 写一个指向 exe 的 .lnk
fn write_lnk(lnk: &std::path::Path, exe: &std::path::Path) -> windows::core::Result<()> {
    unsafe {
        // 已初始化（STA/MTA 都行）就直接用；失败也不拦——ShellLink 在任何 COM 模式下都能创建
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&windows::core::HSTRING::from(exe))?;
        let dir = exe.parent().unwrap_or(exe);
        link.SetWorkingDirectory(&windows::core::HSTRING::from(dir))?;
        let file: IPersistFile = link.cast()?;
        let wide: Vec<u16> = lnk.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        file.Save(PCWSTR(wide.as_ptr()), false)
    }
}

/// 登录自启计划任务：先试免提权创建（部分系统允许），被拒再走 ShellExecute runas
/// 弹一次 UAC（用户取消就算了，还有 Run 键和启动文件夹两条通道兜底）
fn set_scheduled_task() {
    let exe = std::env::current_exe().ok().map(|p| p.display().to_string());
    let Some(exe) = exe else { return };
    let args = format!("/create /tn \"{TASK_NAME}\" /tr \"\\\"{exe}\\\"\" /sc onlogon /delay 0000:05 /f");
    // 直接调 schtasks：免提权创建成功就不打扰用户
    let out = std::process::Command::new("schtasks")
        .raw_arg(format!("/create /tn \"{TASK_NAME}\" /tr \"\\\"{exe}\\\"\" /sc onlogon /delay 0000:05 /f"))
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    if let Ok(o) = &out {
        if o.status.success() {
            crate::tlog!("[guard] 计划任务开机启动已创建（登录后延迟 5s）");
            return;
        }
    }
    // 免提权失败：提权重试（UAC 弹窗；ShellExecuteW 是异步的，拿不到 rc，只能看它有没有被接受）
    crate::tlog!("[guard] 计划任务免提权创建失败（{:?}），走提权重试（弹 UAC，取消则跳过）",
        out.as_ref().ok().and_then(|o| o.status.code()));
    let rc = unsafe {
        let mut a: Vec<u16> = args.encode_utf16().collect();
        a.push(0);
        ShellExecuteW(
            None,
            w!("runas"),
            w!("schtasks.exe"),
            PCWSTR(a.as_ptr()),
            None,
            SW_HIDE,
        )
    };
    if rc.0 as u32 > 32 {
        crate::tlog!("[guard] 计划任务提权创建请求已提交（UAC 通过后生效）");
    } else {
        crate::tlog!("[guard] 计划任务提权创建被拒绝（rc={}），仅保留 Run 键 + 启动文件夹两条通道", rc.0 as isize);
    }
}

fn del_scheduled_task() {
    let out = std::process::Command::new("schtasks")
        .raw_arg(format!("/delete /tn \"{TASK_NAME}\" /f"))
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    if let Ok(o) = &out {
        if o.status.success() {
            crate::tlog!("[guard] 计划任务开机启动已删除");
            return;
        }
    }
    // 删除也要管理员（任务是提权建的）：走 runas，用户取消就留着（下次开关再清）
    let args = format!("/delete /tn \"{TASK_NAME}\" /f");
    let mut a: Vec<u16> = args.encode_utf16().collect();
    a.push(0);
    let rc = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            w!("schtasks.exe"),
            PCWSTR(a.as_ptr()),
            None,
            SW_HIDE,
        )
    };
    crate::tlog!(
        "[guard] 计划任务删除{}（rc={}）",
        if rc.0 as u32 > 32 { "提权请求已提交" } else { "失败" },
        rc.0 as isize
    );
}

/// 开机后经过的秒数（诊断"开机自启"时间线用）
pub fn uptime_secs() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() / 1000 }
}

fn run_key_wide() -> Vec<u16> {
    RUN_KEY.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------- 退出守护 ----------

/// 主程序 exe：守护自己跑在 orbit-guard.exe 副本里，复活要拉起的是同目录的 orbit.exe
fn main_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let is_main = exe
        .file_name()
        .map(|n| n.eq_ignore_ascii_case("orbit.exe"))
        .unwrap_or(false);
    if is_main {
        return Some(exe);
    }
    let sibling = exe.with_file_name("orbit.exe");
    if sibling.is_file() { Some(sibling) } else { Some(exe) }
}

/// 守护用独立副本 `orbit-guard.exe`：任务管理器里与主程序不同名、不会归成一组。
/// 之前守护和主程序同名（都是 orbit.exe），任务管理器「结束任务」会把两个一起杀掉，
/// 复活永远轮不上——实测踩过。副本缺失或比主程序旧就刷新；刷新失败（旧副本
/// 正在跑被锁）沿用现有副本，再不行退回主程序本体。
fn guardian_exe() -> Option<PathBuf> {
    let exe = main_exe()?;
    let guard = exe.with_file_name("orbit-guard.exe");
    let stale = match (
        std::fs::metadata(&exe).and_then(|m| m.modified()),
        std::fs::metadata(&guard).and_then(|m| m.modified()),
    ) {
        (Ok(src), Ok(dst)) => src > dst,
        (Ok(_), Err(_)) => true,
        _ => false,
    };
    if stale {
        match std::fs::copy(&exe, &guard) {
            Ok(_) => crate::tlog!("[guard] 守护副本已刷新：{}", guard.display()),
            Err(e) => crate::tlog!("[guard] 守护副本刷新失败（{e}），沿用现有副本"),
        }
    }
    if guard.is_file() { Some(guard) } else { Some(exe) }
}

/// 主进程启动时调用：拉起守护子进程（默认开；在设置里关掉后不再拉起）。
/// 复活出来的 orbit 带 ORBIT_NO_GUARDIAN=1：守护已经存在（就是拉起它的那个），
/// 不再嵌套拉起；全新启动（开机自启 / 手动启动）顺手把复活计数清零。
pub fn spawn_guardian() {
    if std::env::var_os("ORBIT_NO_GUARDIAN").is_some() {
        crate::tlog!("[guard] 本进程是复活拉起的，沿用既有守护，不再嵌套");
        return;
    }
    set_revive_count(0);
    if !crate::settings::guardian() {
        return;
    }
    let Some(exe) = guardian_exe() else { return };
    let pid = std::process::id();
    std::thread::Builder::new()
        .name("guard-spawn".into())
        .spawn(move || {
            match std::process::Command::new(&exe)
                .arg(format!("--guardian={pid}"))
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
            {
                Ok(_) => crate::tlog!(
                    "[guard] 守护子进程已启动：{}（父 pid={pid}）",
                    exe.display()
                ),
                Err(e) => crate::tlog!("[guard] 守护子进程启动失败：{e}"),
            }
        })
        .ok();
}

/// 复活：重新拉起 orbit（不建控制台）。复活出来的进程带 ORBIT_NO_GUARDIAN=1，
/// 不会再自己拉守护——由本守护进程继续盯它、必要时再复活。
fn spawn_orbit() -> Option<std::process::Child> {
    let exe = main_exe()?;
    std::process::Command::new(&exe)
        .env("ORBIT_NO_GUARDIAN", "1")
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .ok()
}

// ---------- 复活计数 ----------

fn revive_count() -> u32 {
    std::fs::read_to_string(crate::store::path(REVIVE_FILE))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn set_revive_count(n: u32) {
    let _ = std::fs::write(crate::store::path(REVIVE_FILE), n.to_string());
}

/// 守护子进程：父进程无响应时的"保险丝"。
///
/// 对齐 Atrium「守护进程约每 4 秒检查宿主健康状况；主进程无响应，就把 Explorer
/// 任务栏重新排到宿主之前」：
///   - 每 4s 用 `SendMessageTimeout` 探一下 orbit 主窗口。窗口消失或 600ms 不应答
///     就算失去响应（卡死 / 宿主线程被挂），此时把真任务栏抬到假宿主之前，
///     让系统托盘重新可点、图标注册回到 Explorer；
///   - orbit 一旦恢复响应，把自己的假宿主重新抬回最前（guard_loop 会持续做，这里
///     只补一次），不干扰 orbit 的正常接管；
///   - orbit 退出后按 marker 分类（clean / panic / 无标记），panic 与异常退出走
///     复活流程（见 `run_guardian`），最多连续 3 次，成功后计数清零。
const HEALTH_POLL_MS: u32 = 4_000;
const PROBE_TIMEOUT_MS: u32 = 600;

/// 给父进程主窗口发 WM_NULL 探活：能应答说明父进程消息循环还活着。
unsafe fn parent_alive(parent: u32) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, SendMessageTimeoutW, SMTO_ABORTIFHUNG};
    use windows::core::PCWSTR;
    let mut cur = windows::Win32::Foundation::HWND::default();
    loop {
        let Ok(h) = FindWindowExW(
            windows::Win32::Foundation::HWND::default(),
            cur,
            windows::core::w!("ORBIT_CAPSULE"),
            PCWSTR::null(),
        ) else {
            break;
        };
        cur = h;
        let mut pid = 0u32;
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(h, Some(&mut pid));
        if pid != parent {
            continue;
        }
        let mut result: usize = 0;
        let ok = SendMessageTimeoutW(
            h,
            windows::Win32::UI::WindowsAndMessaging::WM_NULL,
            windows::Win32::Foundation::WPARAM(0),
            windows::Win32::Foundation::LPARAM(0),
            SMTO_ABORTIFHUNG,
            PROBE_TIMEOUT_MS,
            Some(&mut result),
        );
        return ok.0 != 0;
    }
    false
}

/// 父进程无响应时把真任务栏抬到假宿主之前；恢复后把假宿主抬回最前。
unsafe fn health_tick(parent: u32, alive: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, HWND_TOPMOST, SetWindowPos, SWP_NOMOVE, SWP_NOSIZE, SWP_NOACTIVATE};
    use windows::core::{w, PCWSTR};
    let mut cur = windows::Win32::Foundation::HWND::default();
    loop {
        let Ok(h) = FindWindowExW(
            windows::Win32::Foundation::HWND::default(),
            cur,
            w!("Shell_TrayWnd"),
            PCWSTR::null(),
        ) else {
            break;
        };
        cur = h;
        let mut pid = 0u32;
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(h, Some(&mut pid));
        if alive {
            // 父进程恢复：把假宿主（pid==parent）抬回去
            if pid == parent {
                let _ = SetWindowPos(
                    h, HWND_TOPMOST, 0, 0, 0, 0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        } else if pid != parent && pid != 0 {
            // 父进程卡死：把 Explorer 真任务栏抬到最前
            let mut base = exe_base(pid);
            if base == "explorer.exe" {
                let _ = SetWindowPos(
                    h, HWND_TOPMOST, 0, 0, 0, 0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                crate::tlog!("[guard] 父进程无响应，已把 Explorer 任务栏抬到宿主之前");
                base.clear();
            }
        }
    }
}

fn exe_base(pid: u32) -> String {
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(proc) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            proc,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        ).is_ok();
        let _ = CloseHandle(proc);
        if !ok || len == 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
            .rsplit(['\\', '/']).next().unwrap_or("").to_lowercase()
    }
}

/// 守护子进程入口：盯住 orbit 退出，异常/panic 退出时复活它（连续最多 3 次），
/// 并做健康巡检。复活出来的 orbit 由本进程继续盯。此进程不建任何窗口，跑完即退。
pub fn run_guardian(parent: u32) {
    unsafe {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        };
        crate::tlog!("[guard] 守护子进程就绪，盯住 orbit pid={parent}");
        let mut pid = parent;
        // 复活拉起的 orbit 的 Child：留着句柄供等待；None = 盯的是外部拉起的进程
        let mut child: Option<std::process::Child> = None;
        loop {
            // 当前盯住的进程句柄
            let handle: HANDLE = match child.as_ref() {
                Some(c) => HANDLE(c.as_raw_handle()),
                None => {
                    let Ok(h) = OpenProcess(
                        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                        false,
                        pid,
                    ) else {
                        crate::tlog!("[guard] 打不开 orbit 进程句柄（pid={pid}），守护退出");
                        return;
                    };
                    h
                }
            };
            let mut was_alive = true;
            loop {
                let wait = WaitForSingleObject(handle, HEALTH_POLL_MS);
                if wait == WAIT_OBJECT_0 {
                    break; // orbit 退出
                }
                if wait == WAIT_TIMEOUT {
                    let alive = parent_alive(pid);
                    if alive != was_alive {
                        crate::tlog!(
                            "[guard] 健康巡检：orbit{}响应",
                            if alive { "恢复" } else { "无" }
                        );
                        health_tick(pid, alive);
                        was_alive = alive;
                    }
                    continue;
                }
                crate::tlog!("[guard] 等待 orbit 进程失败，守护退出");
                if child.is_none() { let _ = CloseHandle(handle); }
                return;
            }
            if child.is_none() { let _ = CloseHandle(handle); }
            // 退出原因分类：clean = 正常退出；panic = panic（任务栏已恢复）；无标记 = 异常
            match marker_kind().as_deref() {
                Some("clean") => {
                    crate::tlog!("[guard] orbit 正常退出（任务栏已恢复），守护退出");
                    set_revive_count(0);
                    return;
                }
                Some("panic") => {
                    crate::tlog!("[guard] orbit panic 退出（panic 钩子已恢复任务栏），准备复活");
                }
                _ => {
                    crate::tlog!("[guard] orbit 异常退出：先恢复任务栏，再尝试复活");
                    crate::hide::force_restore();
                }
            }
            // 复活：连续最多 3 次；复活出来的 orbit 稳定跑满 60s 就把计数清零
            let count = revive_count();
            if count >= MAX_REVIVE {
                crate::tlog!("[guard] 连续复活 {MAX_REVIVE} 次仍异常退出，放弃复活");
                if marker_kind().is_none() {
                    // 只有异常退出才可能把任务栏停在隐藏态，重启 explorer 兜底
                    std::thread::sleep(Duration::from_millis(800));
                    let rc = std::process::Command::new("cmd")
                        .args(["/c", "taskkill /f /im explorer.exe && start explorer.exe"])
                        .creation_flags(CREATE_NO_WINDOW)
                        .status();
                    crate::tlog!("[guard] explorer 重启命令 rc={rc:?}");
                }
                return;
            }
            set_revive_count(count + 1);
            crate::tlog!("[guard] 复活 orbit（第 {}/{} 次）", count + 1, MAX_REVIVE);
            let Some(c) = spawn_orbit() else {
                crate::tlog!("[guard] 复活失败：拉起 orbit.exe 出错，守护退出");
                return;
            };
            pid = c.id();
            child = Some(c);
            // 成功判定：60s 内没再退出 = 复活成功，计数清零；之后继续盯住新 orbit
            let h = HANDLE(child.as_ref().unwrap().as_raw_handle());
            if WaitForSingleObject(h, REVIVE_SUCCESS_MS as u32) == WAIT_TIMEOUT {
                crate::tlog!("[guard] 复活成功（新 orbit 已稳定运行 60s），复活计数清零");
                set_revive_count(0);
            } else {
                crate::tlog!("[guard] 复活出来的 orbit 在 60s 内又退出了");
                // 回到循环：按标记分类，可能继续复活（计数已 +1），或放弃
            }
        }
    }
}

/// 外部关闭主窗口（任务管理器「结束任务」对有窗口的程序会先发 WM_CLOSE）。
/// 这不是 Ctrl+Alt+O 的正常退出，按可复活处理。
static EXTERNAL_CLOSE: AtomicBool = AtomicBool::new(false);

/// 主窗口收到 WM_DESTROY（外部关闭路径）时调用：收尾时留可复活标记
pub fn note_external_close() {
    EXTERNAL_CLOSE.store(true, Ordering::SeqCst);
}

/// 主进程收尾统一走这里：外部关闭 → 留 panic 类标记（守护不重启 explorer，
/// 但会复活）；否则 = 正常退出（Ctrl+Alt+O）→ clean 标记 + 清零复活计数。
pub fn finish_exit() {
    if EXTERNAL_CLOSE.load(Ordering::SeqCst) {
        crate::tlog!("[guard] 主窗口被外部关闭（非 Ctrl+Alt+O），留可复活标记");
        mark_panic_exit();
    } else {
        mark_clean_exit();
    }
}

/// orbit 正常退出（Ctrl+Alt+O / 还原后退出）时留 clean 标记 + 清零复活计数，
/// 让守护子进程静默收场
pub fn mark_clean_exit() {
    write_marker("clean");
    set_revive_count(0);
}

/// orbit panic 退出时留 panic 标记：panic 钩子已经恢复了任务栏，
/// 守护不用再动 explorer，但要复活 orbit。
pub fn mark_panic_exit() {
    write_marker("panic");
}

fn write_marker(kind: &str) {
    let p = crate::store::path(MARKER);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_default();
    let _ = std::fs::write(p, format!("{kind} {now}"));
}

/// 标记是否新鲜；返回 Some("clean"/"panic")，None = 无标记/过期（异常退出）。
/// 旧版本标记只有时间戳，按 clean 处理。
fn marker_kind() -> Option<String> {
    let p = crate::store::path(MARKER);
    let Ok(m) = std::fs::metadata(&p) else { return None };
    let Ok(modified) = m.modified() else { return None };
    let Ok(age) = modified.elapsed() else { return None };
    if age.as_millis() as u64 > MARKER_FRESH_MS {
        return None;
    }
    let Ok(s) = std::fs::read_to_string(&p) else {
        return Some("clean".to_string());
    };
    let first = s.split_whitespace().next().unwrap_or("");
    if first.parse::<u64>().is_ok() {
        Some("clean".to_string())
    } else if first.is_empty() {
        Some("clean".to_string())
    } else {
        Some(first.to_string())
    }
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
