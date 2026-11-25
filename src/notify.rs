//! 内嵌提示音：右下角横幅弹出时，Orbit 侧用 MCI 播放一段嵌进二进制的提示音
//! ——**不修改微信本体**，微信自己的声音/弹窗照旧。
//!
//! 实现：`include_bytes!` 把 mp3 编译进 exe；首次播放时释放到
//! `<exe>/data/wechat_notify.mp3`，然后走 MCI（`mciSendStringW`，DirectShow
//! 后端，免第三方解码库）。alias 打开一次复用，之后每次触发 seek 回头再播。
//!
//! 关键点：**单条播放线程 + 队列**。同时来好几条横幅时，不再是"后一声打断前一声"
//! （听感上等于漏响），而是排队逐条完整播完——每弹一条横幅就实打实响一次。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::Multimedia::mciSendStringW;

/// 嵌入的提示音（约 36 KB）
static SOUND_MP3: &[u8] = include_bytes!("../assets/wechat_notify.mp3");
/// 排队上限：突然来一大批消息时最多排这么多声，避免提示音拖到很久以后还在响
const MAX_PENDING: u64 = 8;
/// MCI alias（进程内唯一）
const ALIAS: &str = "orbit_notify";

/// 正在排队的播放请求数（用来限制队列长度）
static PENDING: AtomicU64 = AtomicU64::new(0);
/// 播放队列：只有一个消费线程，保证每一声都完整播完。
/// 载荷是这一声要重复的倍数（1 = 单次；>1 = 循环 N 次；u32::MAX = 直到 stop）。
static PLAY_TX: OnceLock<Sender<u32>> = OnceLock::new();
/// "直到消息消失"模式的停止标记：横幅消失时由调用方置位，worker 循环播放到它为真
static UNTIL_STOP: AtomicBool = AtomicBool::new(false);

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

/// 右下角横幅弹出时播放的提示音：**每弹一条就排一声**，不再做时间合并。
/// 是否响、响几遍由 `sound_mode` 设置决定（关 / 单次 / 2 次 / 3 次 / 直到消失）。
pub fn play_banner() {
    enqueue("横幅");
}

/// 当前提示音是否开启。兼容旧键：settings.txt 没有 sound_mode 时按 wechat_sound。
fn sound_on() -> bool {
    !matches!(crate::settings::sound_mode(), crate::settings::SoundMode::Off)
}

/// 把 SoundMode 换成 worker 的重复次数（u32::MAX = 循环到 stop）
fn repeat_count() -> u32 {
    use crate::settings::SoundMode::*;
    match crate::settings::sound_mode() {
        Off => 0,
        Once => 1,
        Twice => 2,
        Thrice => 3,
        UntilDismiss => u32::MAX,
    }
}

/// 横幅消失时调用：终止"直到消息消失"的循环播放
pub fn stop_until_dismiss() {
    UNTIL_STOP.store(true, Ordering::Relaxed);
}

/// 入队一声（按 sound_mode 决定是否真的播、播几遍），真正播放交给单线程 worker
fn enqueue(tag: &str) {
    if !sound_on() {
        return;
    }
    let repeat = repeat_count();
    if repeat == 0 {
        return;
    }
    if repeat == u32::MAX {
        UNTIL_STOP.store(false, Ordering::Relaxed);
    }
    let tx = PLAY_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<u32>();
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
    if tx.send(repeat).is_err() {
        PENDING.fetch_sub(1, Ordering::Relaxed);
        crate::tlog!("[notify] 播放线程不可用，丢弃一声（{tag}）");
        return;
    }
    crate::tlog!("[notify] 提示音入队（{tag}，重复 {repeat}）");
}

/// 播放线程：打开一次 alias，之后每条请求 seek 回头再播，**等这一声播完**才处理下一条。
/// 载荷 repeat：有限值播放 N 遍；u32::MAX 循环播放直到 `UNTIL_STOP` 置位。
fn worker(rx: Receiver<u32>) {
    unsafe {
        crate::tlog!("[notify] 播放线程启动 tid={}", crate::crash::current_thread_id());
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

        while let Ok(repeat) = rx.recv() {
            PENDING.fetch_sub(1, Ordering::Relaxed);
            let until_stop = repeat == u32::MAX;
            let mut played = 0u32;
            loop {
                let _ = send(&format!("seek {ALIAS} to start"));
                if send(&format!("play {ALIAS}")) != 0 {
                    crate::tlog!("[notify] MCI play 失败");
                    break;
                }
                // 等这一声真正播完：MCI 报 stopped 就继续下一遍，
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
                        let _ = send(&format!("stop {ALIAS}"));
                        break;
                    }
                }
                played += 1;
                if until_stop {
                    if UNTIL_STOP.load(Ordering::Relaxed) {
                        let _ = send(&format!("stop {ALIAS}"));
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(300)); // 两声之间留点间隔
                } else if played >= repeat {
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
