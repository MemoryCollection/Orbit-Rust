//! 跨线程「设置已应用」即时通知。
//!
//! 设置页在独立的 eframe 线程里跑，点「应用」后 `settings::commit_edit()` 只做
//! `save()+bump()`；胶囊 / Dock 原本靠各自的 500ms 定时器轮询版本号才重绘，
//! 所以主题切换、模块显隐变化时最长要等半秒才看到效果。
//!
//! 这里让设置页在提交后主动给两个窗口各投一条 `WM_SETTINGS_APPLIED`，
//! 收到就立刻走一次「版本变化」的刷新路径，把延迟压到一帧之内。
//! 定时器轮询保留作兜底（手改 settings.txt 仍靠它）。

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// 设置页提交 / 还原后投递：收到方立即按新版本重绘。
pub const WM_SETTINGS_APPLIED: u32 = WM_APP + 0x72;

/// 设置页（独立子进程）请求主进程「手动拿一次微信 UIA」。
/// 跨进程投递到胶囊窗口，主进程收到后走正常抓取流程。
pub const WM_GRAB_UIA: u32 = WM_APP + 0x73;

/// 设置页（独立子进程）发来的横幅大小预览请求：lParam = 滑条原始值 f32 bits。
/// 跨进程投递到横幅窗口（ORBIT_WECARD），主进程收到后写入设置内存并弹预览卡。
pub const WM_BANNER_PREVIEW: u32 = WM_APP + 0x74;

/// 胶囊主窗口 HWND（raw isize 存，避免跨线程持有 HWND）
static PILL_HWND: AtomicIsize = AtomicIsize::new(0);
/// Dock 窗口 HWND
static DOCK_HWND: AtomicIsize = AtomicIsize::new(0);
/// 天气卡片窗口 HWND
static WEATHER_HWND: AtomicIsize = AtomicIsize::new(0);

/// 窗口 init 时登记自己的 HWND（raw 值，线程安全）。
pub fn register_pill(hwnd: HWND) {
    PILL_HWND.store(hwnd.0 as isize, Ordering::Release);
}

pub fn register_dock(hwnd: HWND) {
    DOCK_HWND.store(hwnd.0 as isize, Ordering::Release);
}

pub fn register_weather(hwnd: HWND) {
    WEATHER_HWND.store(hwnd.0 as isize, Ordering::Release);
}

/// 设置页提交 / 还原后调用：给已登记的窗口各投一条刷新消息。
/// PostMessage 是线程安全的，可从 eframe 线程直接调用；窗口尚未登记时跳过。
pub fn notify_applied() {
    for raw in [
        PILL_HWND.load(Ordering::Acquire),
        DOCK_HWND.load(Ordering::Acquire),
        WEATHER_HWND.load(Ordering::Acquire),
    ] {
        if raw == 0 {
            continue;
        }
        unsafe {
            let h = HWND(raw as *mut core::ffi::c_void);
            let _ = PostMessageW(h, WM_SETTINGS_APPLIED, WPARAM(0), LPARAM(0));
        }
    }
}
