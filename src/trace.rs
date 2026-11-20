//! 运行日志。发布版带 `windows_subsystem = "windows"`（不弹控制台），
//! 所以诊断输出统一写到 `<exe 目录>/orbit.log`，进程启动时清空。
//! 设 `ORBIT_TRACE=1` 时额外记录逐轮扫描/重绘的细节（`vlog!`），平时只记错误（`tlog!`）。

use std::io::Write;

use std::sync::{Mutex, OnceLock};

static LOG: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

/// 打开（并清空）日志文件；在 main 最前面调用一次。
pub fn init() {
    let path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("orbit.log")))
        .unwrap_or_else(|| std::path::PathBuf::from("orbit.log"));
    let file = std::fs::File::create(path).ok();
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
