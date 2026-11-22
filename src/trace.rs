//! 运行日志。发布版带 `windows_subsystem = "windows"`（不弹控制台），
//! 所以诊断输出统一写到 `<exe 目录>/` 下的日志文件。
//! 设 `ORBIT_TRACE=1` 时额外记录逐轮扫描/重绘的细节（`vlog!`），平时只记错误（`tlog!`）。

use std::io::Write;

use std::sync::{Mutex, OnceLock};

static LOG: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

/// 单个日志文件超过这个大小就在下次启动时轮转（重建）
const LOG_ROTATE_BYTES: u64 = 2_000_000;

/// 打开日志文件；在 main 最前面调用一次。
/// - 主进程：`orbit.log`
/// - 守护子进程（`--guardian=`）：独立的 `orbit-guard.log`（追加写，主进程重启不丢守护现场）
/// - 设置页子进程（`--settings-ui`）：独立的 `orbit-settings.log`
/// 主进程/守护都**追加写**（超 2MB 才重建）：以前每次启动清空，会把开机自启失败、
/// 上一次异常退出的现场日志冲掉，出了事永远查不到第一现场。
pub fn init() {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_default();
    let is_guardian = std::env::args().any(|a| a.starts_with("--guardian="));
    let is_child = std::env::args().any(|a| a == "--settings-ui");
    let name = if is_guardian {
        "orbit-guard.log"
    } else if is_child {
        "orbit-settings.log"
    } else {
        "orbit.log"
    };
    let path = dir.join(name);
    let file = if is_child {
        // 设置页每次打开都是新进程，保留清空语义（单次会话的日志）
        std::fs::File::create(&path).ok()
    } else {
        let oversized = std::fs::metadata(&path)
            .map(|m| m.len() > LOG_ROTATE_BYTES)
            .unwrap_or(false);
        if oversized {
            let _ = std::fs::remove_file(&path);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
    };
    let _ = LOG.set(Mutex::new(file));
}

/// 是否开启了详细跟踪
pub fn verbose() -> bool {
    std::env::var_os("ORBIT_TRACE").is_some()
}

pub fn write(args: std::fmt::Arguments) {
    let Some(lock) = LOG.get() else { return };
    let Ok(mut guard) = lock.lock() else { return };
    let Some(f) = guard.as_mut() else { return };
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let _ = writeln!(
        f,
        "[{:02}:{:02}:{:02}.{:03}] {}",
        t.wHour, t.wMinute, t.wSecond, t.wMilliseconds, args
    );
}

/// 始终记录（错误/生命周期）
#[macro_export]
macro_rules! tlog {
    ($($t:tt)*) => { $crate::trace::write(format_args!($($t)*)) };
}

/// 只在 ORBIT_TRACE=1 时记录
#[macro_export]
macro_rules! vlog {
    ($($t:tt)*) => {
        if $crate::trace::verbose() {
            $crate::trace::write(format_args!($($t)*))
        }
    };
}
