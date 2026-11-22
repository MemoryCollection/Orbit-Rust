//! Dock 内建的「系统控制」按钮：音量 / 亮度 / WiFi / 蓝牙。
//!
//! 图标用 Orbit 自己的 `assets/orbit-icon.png`（找不到时退回 exe 内嵌图标）；
//! 点击时把内嵌的 `SystemControl.ps1` 释放到 data 目录（跟提示音同一位置，
//! 内容变了自动覆盖），再用 Windows PowerShell 启动——脚本是 WPF 自绘面板，
//! `-STA -WindowStyle Hidden` 保证窗口正常显示且不带黑框控制台。
//!
//! 运行中的面板窗口属于 powershell.exe，Dock 的运行项去重靠
//! [`process_key`] 把它归到这个内建按钮上：面板已开时点按钮 = 切到面板。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::store;

/// 编译进 exe 的面板脚本（WPF 自绘：音量 / 亮度 / WiFi 扫描连接 / 蓝牙跳设置）
const SCRIPT: &str = include_str!("../tools/system-control/SystemControl.ps1");

/// 面板进程：Dock 运行项去重 / 前台指示条都要按它对齐
static PS_KEY: LazyLock<String> = LazyLock::new(|| {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    format!("{root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe").to_lowercase()
});

/// (图标边长 px) → Orbit 图标 RGBA。dock 换图标缩放后按新尺寸重取，按 px 缓存。
static ICON_CACHE: LazyLock<Mutex<HashMap<i32, Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// powershell.exe 的 exe 全路径（小写），运行项去重用的 key
pub fn process_key() -> String {
    PS_KEY.clone()
}

/// 内嵌脚本释放路径：exe 的 data 目录（`store::path`，跟提示音同一位置）
fn script_path() -> PathBuf {
    store::path("SystemControl.ps1")
}

/// 确保脚本已释放到 data 目录。内容变了就覆盖，改脚本不用手动删旧文件。
fn ensure_script() -> Option<PathBuf> {
    let p = script_path();
    // include_str! 的文件可能自带 BOM，先剥掉再统一补一个：
    // PowerShell 5.1 没有 BOM 时把 UTF-8 当 ANSI 读，中文界面会乱码
    let script = SCRIPT.trim_start_matches('\u{feff}');
    let mut body = String::with_capacity(script.len() + 3);
    body.push('\u{feff}');
    body.push_str(script);
    let stale = match std::fs::read(&p) {
        Ok(bytes) => {
            let s = String::from_utf8_lossy(&bytes);
            s.trim_start_matches('\u{feff}') != script
        }
        Err(_) => true,
    };
    if stale {
        if let Err(e) = std::fs::write(&p, body) {
            crate::tlog!("[syspanel] 脚本释放失败：{e}");
            return None;
        }
    }
    Some(p)
}

/// orbit-icon.png 的运行期位置：发布目录 → exe 旁边 → 开发期仓库 assets。
/// 都没有就退回 exe 自身内嵌的图标（build.rs 打进去的 orbit.ico）。
fn icon_source() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Some(d) = exe.parent() {
        cands.push(d.join("orbit-icon.png"));
        cands.push(d.join("assets").join("orbit-icon.png"));
        // target/debug/orbit.exe → 仓库 assets/
        cands.push(d.join("../../assets").join("orbit-icon.png"));
    }
    cands.push(PathBuf::from("assets").join("orbit-icon.png"));
    let hit = cands.into_iter().find(|p| p.exists());
    hit.or_else(|| Some(exe))
}

/// Orbit 图标（straight RGBA）。取不到返回空，调用方跳过这个按钮。
pub fn icon(px: i32) -> Vec<u8> {
    if let Ok(m) = ICON_CACHE.lock() {
        if let Some(v) = m.get(&px) {
            return v.clone();
        }
    }
    let Some(p) = icon_source() else { return Vec::new() };
    let rgba = crate::dock::large_icon_of_path(&p, px);
    if !rgba.is_empty() {
        if let Ok(mut m) = ICON_CACHE.lock() {
            m.insert(px, rgba.clone());
        }
    }
    rgba
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 点击 Dock 上的「系统控制」按钮：释放脚本并启动面板。
/// 面板已在跑时 Dock 的 wins 判断会先走"切窗口"，一般到不了这里。
pub fn launch() {
    let Some(script) = ensure_script() else { return };
    let ps = PS_KEY.to_string();
    let args = format!(
        "-NoProfile -ExecutionPolicy Bypass -STA -WindowStyle Hidden -File \"{}\"",
        script.display()
    );
    let exe_w = to_wide(&ps);
    let args_w = to_wide(&args);
    unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(exe_w.as_ptr()),
            PCWSTR(args_w.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
    crate::tlog!("[syspanel] 已启动系统控制面板：{}", script.display());
}
