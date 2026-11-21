//! 内嵌微信提示音：微信托盘图标开始闪烁（新消息提醒）时，Orbit 侧用 MCI 播放
//! 一段嵌进二进制的提示音——**不修改微信本体**，微信自己的声音/弹窗照旧。
//!
//! 实现：`include_bytes!` 把 mp3 编译进 exe；首次播放时释放到
//! `<exe>/data/wechat_notify.mp3`，然后走 MCI（`mciSendStringW`，DirectShow
//! 后端，免第三方解码库）。alias 打开一次复用，之后每次触发 seek 回头再播。
//! 闪烁是 250ms 亮/灭交替，调用侧认每次"亮→灭"（`flash_at` 刷新）触发一声，
//! 连续消息每闪都响；这里只做极短的重入兜底。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::Multimedia::mciSendStringW;

/// 嵌入的提示音（约 36 KB）
static SOUND_MP3: &[u8] = include_bytes!("../assets/wechat_notify.mp3");
/// 播放之间的极短兜底间隔（毫秒）：调用侧已用每次闪烁的 flash_at 精确去重，
/// 这里只防同一帧重入（比如 render 和 70ms 定时器在同一灭帧各调一次）。
/// 微信一闪约 250ms，两次闪烁间隔 ~500ms，200ms 兜底不会吞掉下一次闪烁。
const MIN_GAP_MS: u64 = 200;
/// MCI alias（进程内唯一）
const ALIAS: &str = "orbit_notify";

/// 每个图标 key 各自的上次播放时刻：微信双开时两个图标各算各的一轮提醒，
/// 全局节流会把第二个实例的提醒吞掉（用户实测）
static LAST_PLAY: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
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

/// 播放提示音。调用侧每次闪烁（亮→灭）触发一次，连续消息每闪都响；
/// 这里按图标 key 做 200ms 重入兜底，双开的两个实例互不干扰。
pub fn play_wechat(key: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    {
        let Ok(mut last) = LAST_PLAY.lock() else { return };
        // 顺手清掉过期项，免得 map 无限长（图标 key 有限，其实涨不起来）
        last.retain(|_, t| now.saturating_sub(*t) < MIN_GAP_MS * 2);
        if let Some(prev) = last.get(key) {
            if now.saturating_sub(*prev) < MIN_GAP_MS {
                return;
            }
        }
        last.insert(key.to_string(), now);
    }
    crate::tlog!("[notify] 提示音触发 key={key}");
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
