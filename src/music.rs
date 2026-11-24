//! 音乐数据层：通过 SMTC（GlobalSystemMediaTransportControlsSessionManager）
//! 读取系统正在播放的曲目、播放进度与专辑封面，并控制播放/切歌。采样与控制在独立 MTA 线程里跑。
//!
//! 进度对齐（歌词要用）：部分播放器（如 lx-music）只在换曲/暂停时写一次
//! `Position`，正常播放时它是冻结的，`LastUpdatedTime` 记住那一刻。
//! 所以线程里维护一个"播放头"：以 Position 为锚点，锚点没变就按本地时钟推进，
//! 变了（seek / 恢复播放 / 正常推进）就重新对锚。两次采样之间再由
//! `position_ms()` 外推，供 200ms 的歌词刷帧使用。
//!
//! 封面：SMTC 的 `MediaProperties.Thumbnail` 是 `IRandomAccessStreamReference`，
//! 只在换曲时读一次（读成编码后的图片字节），交给界面层解码成位图。

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager as Manager;
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status;
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;

/// 采样间隔（毫秒）
const SAMPLE_MS: u64 = 250;
/// SMTC 的时间单位是 100ns
const TICKS_PER_MS: i64 = 10_000;
/// "Position 是多久以前写的"上限，防止时间线异常时算出离谱的位置
const MAX_STALE_MS: i64 = 6 * 3600 * 1000;
/// 封面字节上限（正常封面几百 KB，超过就当没有，避免异常播放器把内存撑爆）
const MAX_ART_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, PartialEq)]
pub struct Now {
    pub title: String,
    pub artist: String,
    pub playing: bool,
}

/// 封面更新：`Keep` = 沿用上一首的（没换曲）；`Set` = 换曲时的一次尝试结果（可能是"没有封面"）
enum Art {
    Keep,
    Set(Option<Vec<u8>>),
}

/// 一次采样：当前曲目 + 采样时刻的播放位置（毫秒）+ 封面更新
struct Sample {
    now: Option<Now>,
    pos_ms: i64,
    art: Art,
}

#[derive(Clone, Copy)]
pub enum Cmd {
    Toggle,
    Next,
}

pub struct Music {
    pub now: Option<Now>,
    /// 当前曲目的专辑封面（SMTC 给的原始图片字节，PNG/JPEG 等，由界面层解码）
    pub art: Option<Vec<u8>>,
    /// 封面版本号：每换一次曲目 +1（即使新曲目没有封面也算一次），界面据此重建位图
    pub art_ver: u64,
    /// 最近一次采样到的播放位置（毫秒）与其采样时刻
    pos_ms: i64,
    pos_at: Instant,
    rx: Receiver<Sample>,
    tx: Sender<Cmd>,
}

impl Music {
    pub fn spawn() -> Self {
        let (tx_state, rx_state) = channel();
        let (tx_cmd, rx_cmd) = channel();
        std::thread::Builder::new()
            .name("orbit-music".into())
            .spawn(move || worker(rx_cmd, tx_state))
            .ok();
        Self {
            now: None,
            art: None,
            art_ver: 0,
            pos_ms: 0,
            pos_at: Instant::now(),
            rx: rx_state,
            tx: tx_cmd,
        }
    }

    /// 取走最新采样；返回 true 表示曲目/播放状态/封面变了（界面需要重绘）。
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(s) => {
                    if s.now != self.now {
                        self.now = s.now;
                        changed = true;
                    }
                    if let Art::Set(art) = s.art {
                        self.art = art;
                        self.art_ver = self.art_ver.wrapping_add(1);
                        changed = true;
                    }
                    self.pos_ms = s.pos_ms;
                    self.pos_at = Instant::now();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        changed
    }

    /// 是否在播放（没有会话时为 false）
    pub fn playing(&self) -> bool {
        self.now.as_ref().map(|n| n.playing).unwrap_or(false)
    }

    /// 没有歌词时显示的一行："歌名 · 歌手"（缺一个就只显示另一个）
    pub fn title_line(&self) -> Option<String> {
        let n = self.now.as_ref()?;
        let (t, a) = (n.title.trim(), n.artist.trim());
        match (t.is_empty(), a.is_empty()) {
            (false, false) => Some(format!("{t} · {a}")),
            (false, true) => Some(t.to_string()),
            (true, false) => Some(a.to_string()),
            (true, true) => None,
        }
    }

    /// 当前播放位置（毫秒）：采样值 + 采样之后流逝的时间；暂停时停在采样值
    pub fn position_ms(&self) -> i64 {
        let playing = self.now.as_ref().map(|n| n.playing).unwrap_or(false);
        if playing {
            self.pos_ms + self.pos_at.elapsed().as_millis() as i64
        } else {
            self.pos_ms
        }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

/// 采样线程里的播放头。
/// 以播放器给的 `Position` 为锚点，锚点之间用本地时钟推进：
/// 既能跟上逐帧推进的播放器，也能跟上只在换曲/暂停时写一次的播放器。
#[derive(Default)]
struct Clock {
    playhead: i64,
    /// 上次采样到的原始 Position（毫秒）
    raw: i64,
    /// 上次采样时是否在播放
    playing: bool,
    at: Option<Instant>,
}

impl Clock {
    /// 推进播放头。`since_update` = Position 被写入后到现在过了多久
    /// （播放器不推进 Position 时，靠它把已经播过的一段补回来）。
    fn sample(&mut self, raw: i64, since_update: i64, playing: bool, new_track: bool) -> i64 {
        let now = Instant::now();
        let dt = self.at.map(|t| now.duration_since(t).as_millis() as i64).unwrap_or(0);
        self.at = Some(now);

        if new_track {
            // 刚切到这个曲目：Position 冻结在开头时，用更新时间补上已经播过的一段
            self.playhead = raw.max(0) + if playing { since_update } else { 0 };
        } else if raw != self.raw {
            // 播放器给了新的 Position（seek / 正常推进 / 恢复播放）
            self.playhead = raw.max(0);
        } else if playing && self.playing {
            // Position 没动：按本地时钟继续走（暂停时不走）
            self.playhead += dt;
        }
        self.raw = raw;
        self.playing = playing;
        self.playhead.max(0)
    }
}

fn filetime_now() -> u64 {
    let ft = unsafe { GetSystemTimeAsFileTime() };
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// 一次采样的原始信息
struct Raw {
    now: Option<Now>,
    /// 播放器给的 Position（毫秒）
    pos_ms: i64,
    /// Position 是多久以前写入的（毫秒）
    since_update_ms: i64,
    playing: bool,
    /// 专辑封面引用（按需才 OpenRead，换曲时才读一次）
    thumb: Option<IRandomAccessStreamReference>,
}

impl Raw {
    fn none() -> Self {
        Raw { now: None, pos_ms: 0, since_update_ms: 0, playing: false, thumb: None }
    }
}

fn worker(rx_cmd: Receiver<Cmd>, tx_state: Sender<Sample>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut mgr: Option<Manager> = None;
    let mut clock = Clock::default();
    // 上一个曲目的身份（歌名 + 歌手）；播放/暂停不算换曲
    let mut last_id: Option<(String, String)> = None;
    loop {
        while let Ok(cmd) = rx_cmd.try_recv() {
            if let Some(m) = &mgr {
                control(m, cmd);
            }
        }

        let raw = match &mgr {
            Some(m) => query(m),
            None => match request_manager() {
                Some(m) => {
                    let r = query(&m);
                    mgr = Some(m);
                    r
                }
                None => Raw::none(),
            },
        };

        let id = raw.now.as_ref().map(|n| (n.title.clone(), n.artist.clone()));
        let new_track = id != last_id;
        let pos_ms = clock.sample(raw.pos_ms, raw.since_update_ms, raw.playing, new_track);
        // 换曲才碰封面（读流有开销，采样线程里只做一次）
        let art = if new_track {
            Art::Set(raw.thumb.as_ref().and_then(read_thumb))
        } else {
            Art::Keep
        };
        if new_track {
            // 管理器可能失效（播放器重启），没会话时下次重建
            if raw.now.is_none() && last_id.is_some() {
                mgr = None;
            }
            last_id = id;
        }
        let _ = tx_state.send(Sample { now: raw.now, pos_ms, art });
        std::thread::sleep(Duration::from_millis(SAMPLE_MS));
    }
}

/// 把封面的随机访问流整个读进内存（编码后的图片字节，没做解码）
fn read_thumb(r: &IRandomAccessStreamReference) -> Option<Vec<u8>> {
    let stream = r.OpenReadAsync().ok()?.get().ok()?;
    let size = stream.Size().ok()?;
    if size == 0 || size > MAX_ART_BYTES {
        return None;
    }
    let input = stream.GetInputStreamAt(0).ok()?;
    let reader = DataReader::CreateDataReader(&input).ok()?;
    reader.LoadAsync(size as u32).ok()?.get().ok()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf).ok()?;
    Some(buf)
}

fn request_manager() -> Option<Manager> {
    Manager::RequestAsync().ok()?.get().ok()
}

/// 读当前曲目、时间线与封面引用；没有会话时 now = None
fn query(mgr: &Manager) -> Raw {
    let Ok(session) = mgr.GetCurrentSession() else {
        return Raw::none();
    };
    let status = session
        .GetPlaybackInfo()
        .ok()
        .and_then(|i| i.PlaybackStatus().ok());
    // 播放器正常退出时会把会话标成 Closed，但会话对象可能还留在管理器里，
    // 歌名也还能取到——不当回事的话音乐条就会一直挂着上一次的曲目。
    if status == Some(Status::Closed) {
        return Raw::none();
    }
    let playing = status.map(|s| s == Status::Playing).unwrap_or(false);

    let (pos_ms, since_update_ms) = match session.GetTimelineProperties() {
        Ok(t) => {
            let pos = t.Position().map(|x| x.Duration).unwrap_or(0) / TICKS_PER_MS;
            let since = t
                .LastUpdatedTime()
                .map(|x| {
                    let ms = filetime_now().saturating_sub(x.UniversalTime as u64) / TICKS_PER_MS as u64;
                    (ms as i64).min(MAX_STALE_MS)
                })
                .unwrap_or(0);
            (pos, since)
        }
        Err(_) => (0, 0),
    };

    let Some(props) = session.TryGetMediaPropertiesAsync().ok().and_then(|o| o.get().ok()) else {
        return Raw { now: None, pos_ms, since_update_ms, playing, thumb: None };
    };
    let title = props.Title().map(|s| s.to_string()).unwrap_or_default();
    let artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
    if title.is_empty() && artist.is_empty() {
        return Raw { now: None, pos_ms, since_update_ms, playing, thumb: None };
    }
    // 不是每个播放器都给封面；拿不到就返回 None，界面用占位音符
    let thumb = props.Thumbnail().ok();
    Raw { now: Some(Now { title, artist, playing }), pos_ms, since_update_ms, playing, thumb }
}

fn control(mgr: &Manager, cmd: Cmd) {
    let Ok(session) = mgr.GetCurrentSession() else { return };
    let _ = match cmd {
        Cmd::Toggle => session.TryTogglePlayPauseAsync().and_then(|op| op.get()),
        Cmd::Next => session.TrySkipNextAsync().and_then(|op| op.get()),
    };
}
