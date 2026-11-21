//! 内嵌提示音：微信托盘图标开始闪烁（新消息提醒）或右下角横幅弹出时，Orbit 侧用
//! MCI 播放一段嵌进二进制的提示音——**不修改微信本体**，微信自己的声音/弹窗照旧。
//!
//! 实现：`include_bytes!` 把 mp3 编译进 exe；首次播放时释放到
//! `<exe>/data/wechat_notify.mp3`，然后走 MCI（`mciSendStringW`，DirectShow
//! 后端，免第三方解码库）。alias 打开一次复用，之后每次触发 seek 回头再播。
//!
//! 关键点：**单条播放线程 + 队列**。同时来好几条横幅时，不再是"后一声打断前一声"
//! （听感上等于漏响），而是排队逐条完整播完——每弹一条横幅就实打实响一次。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::Multimedia::mciSendStringW;

/// 嵌入的提示音（约 36 KB）
static SOUND_MP3: &[u8] = include_bytes!("../assets/wechat_notify.mp3");
/// 托盘闪烁这条路径的极短兜底间隔（毫秒）：调用侧已用每次闪烁的 flash_at 精确去重，
/// 这里只防同一帧重入（比如 render 和 70ms 定时器在同一灭帧各调一次）。
/// 微信一闪约 250ms，两次闪烁间隔 ~500ms，200ms 兜底不会吞掉下一次闪烁。
const MIN_GAP_MS: u64 = 200;
/// 排队上限：突然来一大批消息时最多排这么多声，避免提示音拖到很久以后还在响
const MAX_PENDING: u64 = 8;
/// MCI alias（进程内唯一）
const ALIAS: &str = "orbit_notify";

/// 每个图标 key 各自的上次播放时刻：微信双开时两个图标各算各的一轮提醒，
/// 全局节流会把第二个实例的提醒吞掉（用户实测）
static LAST_PLAY: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// 正在排队的播放请求数（用来限制队列长度）
static PENDING: AtomicU64 = AtomicU64::new(0);
/// 播放队列：只有一个消费线程，保证每一声都完整播完
static PLAY_TX: OnceLock<Sender<()>> = OnceLock::new();

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

/// 托盘闪烁提示音。调用侧每次闪烁（亮→灭）触发一次，连续消息每闪都响；
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
    crate::tlog!("[notify] 托盘提示音 key={key}");
    enqueue("托盘闪烁");
}

/// 右下角横幅弹出时播放的提示音：**每弹一条就排一声**，不再做时间合并。
/// 多条横幅同时到达时由播放线程逐条播完（后一声不会截断前一声）。
/// 微信卡片是否响由 `wechat_sound` 设置决定，调用侧在 `silent` 里已判过。
pub fn play_banner() {
    enqueue("横幅");
}

/// 入队一声，真正播放交给单线程 worker（懒启动）
fn enqueue(tag: &str) {
    let tx = PLAY_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("orbit-notify".into())
            .spawn(move || worker(rx));
        if let Err(e) = spawned {
            crate::tlog!("[notify] 播放线程启动失败：{e}");
        }
        tx
    });
    if PENDING.fetch_add(1, Ordering::Relaxed) >= MAX_PENDING {
        PENDING.fetch_sub(1, Ordering::Relaxed);
        crate::tlog!("[notify] 提示音队列已满，丢弃一声（{tag}）");
        return;
    }
    if tx.send(()).is_err() {
        PENDING.fetch_sub(1, Ordering::Relaxed);
        crate::tlog!("[notify] 播放线程不可用，丢弃一声（{tag}）");
        return;
    }
    crate::tlog!("[notify] 提示音入队（{tag}）");
}

/// 播放线程：打开一次 alias，之后每条请求 seek 回头再播，**等这一声播完**才处理下一条。
fn worker(rx: Receiver<()>) {
    unsafe {
        let p = sound_path();
        if !p.exists() {
            if let Err(e) = std::fs::write(&p, SOUND_MP3) {
                crate::tlog!("[notify] 提示音释放失败：{e}");
                return; // 退出后 rx 被丢弃，后续 send 会失败，不会堆积
            }
        }
        if send(&format!("open \"{}\" type mpegvideo alias {ALIAS}", p.display())) != 0 {
            crate::tlog!("[notify] MCI open 失败：{}", p.display());
            return;
        }
        // 音频长度（毫秒）：拿不到就按 1.5s 估，只用于等待超时
        let len = query(&format!("status {ALIAS} length"))
            .trim()
            .parse::<u64>()
            .unwrap_or(1500);
        let deadline = len.saturating_add(1200).clamp(600, 10_000);
        crate::tlog!("[notify] 播放线程就绪，音频长度 {len}ms");

        while rx.recv().is_ok() {
            PENDING.fetch_sub(1, Ordering::Relaxed);
            let _ = send(&format!("seek {ALIAS} to start"));
            if send(&format!("play {ALIAS}")) != 0 {
                crate::tlog!("[notify] MCI play 失败");
                continue;
            }
            // 等这一声真正播完再放下一声：MCI 报 stopped 就立刻继续，
            // 报 playing 就继续等（超时兜底 stop，避免卡死队列）
            let t0 = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(40));
                if !query(&format!("status {ALIAS} mode"))
                    .trim()
                    .eq_ignore_ascii_case("playing")
                {
                    break;
                }
                if t0.elapsed().as_millis() as u64 > deadline {
                    crate::tlog!("[notify] 播放超时，强制 stop");
                    let _ = send(&format!("stop {ALIAS}"));
                    break;
                }
            }
        }
    }
}

/// 发一条 MCI 命令，返回 MCI 错误码（0 = 成功）
unsafe fn send(cmd: &str) -> u32 {
    let mut w: Vec<u16> = cmd.encode_utf16().collect();
    w.push(0);
    mciSendStringW(PCWSTR(w.as_ptr()), None, HWND::default())
}

/// 发一条会返回字符串的 MCI 命令（如 `status alias mode`），失败返回空串
unsafe fn query(cmd: &str) -> String {
    let mut w: Vec<u16> = cmd.encode_utf16().collect();
    w.push(0);
    let mut buf = [0u16; 128];
    let rc = mciSendStringW(PCWSTR(w.as_ptr()), Some(&mut buf[..]), HWND::default());
    if rc != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}
