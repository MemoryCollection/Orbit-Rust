//! 内嵌微信提示音：微信托盘图标开始闪烁（新消息提醒）时，Orbit 侧用 MCI 播放
//! 一段嵌进二进制的提示音——**不修改微信本体**，微信自己的声音/弹窗照旧。
//!
//! 实现：`include_bytes!` 把 mp3 编译进 exe；首次播放时释放到
//! `<exe>/data/wechat_notify.mp3`，然后走 MCI（`mciSendStringW`，DirectShow
//! 后端，免第三方解码库）。alias 打开一次复用，之后每次触发 seek 回头再播。
//! 闪烁是 250ms 亮/灭交替，调用侧只认"从没在闪变在闪"的那一下，这里再加
//! 最小间隔保护，一轮提醒最多响一声。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::Multimedia::mciSendStringW;

/// 嵌入的提示音（约 36 KB）
static SOUND_MP3: &[u8] = include_bytes!("../assets/wechat_notify.mp3");
/// 两次播放的最小间隔（毫秒）：同一轮新消息提醒只响一声
const MIN_GAP_MS: u64 = 3000;
/// MCI alias（进程内唯一）
const ALIAS: &str = "orbit_notify";

static LAST_PLAY: AtomicU64 = AtomicU64::new(0);
/// alias 状态：0 = 还没试过 / 1 = 已打开 / 2 = 打不开（不再重试）
static OPENED: AtomicU64 = AtomicU64::new(0);

/// 这个托盘图标是微信系的吗（按 tooltip 名 + exe 文件名判断，兜底与 Dock 去重同风格）
pub fn is_wechat(name: &str, exe: &str) -> bool {
    let n = name.to_lowercase();
    let e = exe.to_lowercase();
    let base = e.rsplit(['\\', '/']).next().unwrap_or(&e);
    n.contains("weixin")
        || n.contains("wechat")
        || n.contains("微信")
        || base.contains("weixin")
        || base.contains("wechat")
}

fn sound_path() -> PathBuf {
    crate::store::path("wechat_notify.mp3")
}

/// 播放提示音。调用侧已在"图标开始闪"的那一刻触发，这里再加间隔保护。
pub fn play_wechat() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if now.saturating_sub(LAST_PLAY.load(Ordering::Relaxed)) < MIN_GAP_MS {
        return;
    }
    LAST_PLAY.store(now, Ordering::Relaxed);
    std::thread::spawn(|| unsafe {
        if OPENED.load(Ordering::Relaxed) == 2 {
            return; // 之前就没打开成功（缺解码器之类），别反复试
        }
        let p = sound_path();
        if !p.exists() {
            let _ = std::fs::write(&p, SOUND_MP3);
        }
        if OPENED.load(Ordering::Relaxed) == 0 {
            let cmd = format!("open \"{}\" type mpegvideo alias {ALIAS}", p.display());
            let ok = send(&cmd) == 0;
            OPENED.store(u64::from(ok), Ordering::Relaxed);
            if !ok {
                crate::tlog!("[notify] MCI open 失败：{}", p.display());
                return;
            }
        }
        // 从头播放（异步返回）；正在播也直接 seek 回头重播，保证每次都完整响一遍
        let _ = send(&format!("seek {ALIAS} to start"));
        let _ = send(&format!("play {ALIAS}"));
    });
}

/// 发一条 MCI 命令，返回 MCI 错误码（0 = 成功）
unsafe fn send(cmd: &str) -> u32 {
    let mut w: Vec<u16> = cmd.encode_utf16().collect();
    w.push(0);
    mciSendStringW(PCWSTR(w.as_ptr()), None, HWND::default())
}
