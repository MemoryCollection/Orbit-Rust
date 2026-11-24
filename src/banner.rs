//! 通用右下角横幅：任何模块都能用 `banner::show()` 弹一条提示。
//!
//! 独立分层窗口（像 panel.rs 一样 NOACTIVATE + 逐像素透明 + 圆角命中），
//! 不抢焦点；载荷从任意线程用 `PostMessageW(WM_BANNER, lParam=Box<BannerNotice>)`
//! 投进来，窗口过程 `Box::from_raw` 接手。每次弹出都播一次内置提示音。
//!
//! 多条横幅堆叠展示：新的一条在下方、旧的逐个往上递增，各自按设置的秒数停留
//! （设置页「横幅秒数」，0 = 不自动关闭）；
//! 不替换旧内容，点哪张收哪张；微信卡片点一下跳到对应微信窗口（多开按 PID 区分）。
//! 微信消息卡片只是其中一种 `BannerNotice::wechat(...)`；
//! 抓取 UIA 成功/失败、以及以后别的模块想提示，都用 `BannerNotice::info(...)`。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use eframe::egui;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, FindWindowW, GetWindowLongPtrW, GetWindowRect, PostMessageW,
    RegisterClassW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW,
    GWLP_USERDATA, HMENU, HWND_TOPMOST, HTCLIENT, HTTRANSPARENT, IDC_ARROW, LoadCursorW, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, WM_CREATE, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCHITTEST,
    WM_TIMER, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WNDCLASSW, WM_APP,
};

use crate::config as cfg;
use crate::egui_soft::{argb, ellipsize, glass_board, SoftBackend};
use crate::render;
use crate::settings;
use crate::wechatmsg::Reader;

/// 横幅窗口收到后从 lParam 取出 `Box<BannerNotice>` 并展示
pub const WM_BANNER: u32 = WM_APP + 0x51;
/// 横幅窗口收到后从 lParam 取出 `Box<String>`（托盘图标 key），
/// 收起该 key 对应的闪烁横幅（托盘不闪了 → 横幅消失）
pub const WM_BANNER_STOP: u32 = WM_APP + 0x52;

/// 一条横幅载荷（堆分配，通过 lParam 跨线程传；接收方 `Box::from_raw` 接手）。
///
/// 四个字段都是普通文本，绘制方式统一，所以任何模块都能构造：
/// - `tag`   顶行左侧小标签（如 "微信" / "提示"）
/// - `badge` 顶行右侧小字（如 "3 条消息" / "成功"，可空）
/// - `main`  主行（会话名 / 提示正文）
/// - `sub`   副行（消息摘要 / 补充说明，可空）
#[derive(Clone, Default)]
pub struct BannerNotice {
    pub tag: String,
    pub badge: String,
    pub main: String,
    pub sub: String,
    /// true = 这条不响（微信卡片走 `wechat_sound` 设置，避免和托盘闪烁音重复）
    pub silent: bool,
    /// 微信消息的来源进程 PID：点击横幅时跳到该微信窗口；0 = 无跳转目标
    pub pid: u32,
    /// 这条横幅自己的停留秒数：Some(0) = 常驻（不自动关）；None = 用全局
    /// 「横幅秒数」设置。不同模块想用不同秒数就在这里给（设置页只调全局默认）
    pub secs: Option<u32>,
    /// 托盘闪烁来源标记（Dock 的图标 key）：闪烁触发的横幅带着它，
    /// 图标不闪了整条随之收起；普通横幅为 None
    pub flash_key: Option<String>,
    /// 消息来源：wechat / mail / tray / clipboard / other。
    /// 用于按来源选通知通道（Orbit 横幅 / Windows 通知 / 关）和横幅时长。
    pub source: &'static str,
    /// 设置页「横幅大小」滑块的预览样例卡：弹出时把已有的预览卡原位替换
    /// （不会堆出一摞），不进通知通道、不响
    pub preview: bool,
}

impl BannerNotice {
    /// 通用信息横幅（抓取结果、以后其它模块的通知都用它）——每次弹出都响
    pub fn info(tag: &str, main: &str, sub: &str) -> Self {
        Self {
            tag: tag.to_string(),
            badge: String::new(),
            main: main.to_string(),
            sub: sub.to_string(),
            silent: false,
            pid: 0,
            secs: None,
            flash_key: None,
            source: "other",
            preview: false,
        }
    }

    /// 指定消息来源（用于按来源选通知通道与横幅时长）
    pub fn from_source(mut self, source: &'static str) -> Self {
        self.source = source;
        self
    }

    /// 覆盖这条横幅的停留秒数（0 = 常驻不自动关；缺省用该来源的「横幅秒数」）
    pub fn with_secs(mut self, secs: u32) -> Self {
        self.secs = Some(secs);
        self
    }

    /// 微信新消息卡片（接管后的正式卡片与托盘闪烁兜底卡共用）。
    /// 响不响由 `wechat_sound` 设置决定（silent 位），响几遍按 `sound_mode`。
    /// UIA 抓到的信息尽量都上横幅：
    /// - `main`：会话名
    /// - `sub`：摘要（非空才填；空则兜底 "新消息"）
    /// - `badge`：未读数（≥1 都显示，不只 >1）+ 多开时的实例标签（微信1/微信2）
    /// - `pid`：点击横幅跳回对应实例
    pub fn wechat(chat: &str, summary: &str, unread: i32, pid: u32) -> Self {
        let mut badge = String::new();
        if unread > 0 {
            badge = format!("{unread} 条消息");
        }
        if let Some(inst) = crate::wechatmsg::instance_tag(pid) {
            if badge.is_empty() {
                badge = inst;
            } else {
                badge = format!("{badge} · {inst}");
            }
        }
        Self {
            tag: "微信".to_string(),
            badge,
            main: chat.to_string(),
            sub: if summary.is_empty() { "新消息".to_string() } else { summary.to_string() },
            silent: !settings::wechat_sound(),
            pid,
            secs: None,
            flash_key: None,
            source: "wechat",
            preview: false,
        }
    }
}

/// 横幅逻辑尺寸（96 DPI 基准）。1.5 倍：宽 440 → 660，高 92 → 138
const CARD_W: f32 = 660.0;
const CARD_H: f32 = 138.0;
/// 堆叠时相邻横幅的间隙；新条在下方，旧的往上递增
const CARD_GAP: f32 = 12.0;
/// 最多同时显示几条，超出丢最旧的
const MAX_CARDS: usize = 5;
const TIMER_CARD: usize = 1;
const TIMER_CTRL: usize = 2;

/// 窗口句柄放全局（不是 thread_local）：`show()` 会被读取线程、设置页等跨线程调用
static BANNER_HWND: AtomicIsize = AtomicIsize::new(0);
/// 读取线程的启停（横幅窗口的定时器里跟随设置项）
static READER: Mutex<Option<Reader>> = Mutex::new(None);
/// 每个微信图标上次已响应的闪烁时刻：一次"亮→灭"只唤醒一次读取线程
static FLASH_SEEN: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// 兜底卡片已发过的那次闪烁（key → 时刻），同一闪不重复弹卡
static FLASH_CARD_SEEN: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// 闪烁轮次去重（key → 本轮最近一次闪烁时刻）。同一轮连续闪烁（微信亮→灭
/// 周期约 500ms，相邻两闪间隔 <1s）只弹一张横幅、响一声——横幅按
/// 「横幅显示时长」到期收起后图标还在闪也不重弹不重响（否则闪烁不停
/// 声音就不停）；停闪 ≥ FLASH_BURST_GAP_MS 后再闪才算新一轮。
static FLASH_BURST_AT: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// 判定"新一轮闪烁"的最小间隔：距上次闪烁 ≥ 1 秒才再弹再响
const FLASH_BURST_GAP_MS: u128 = 1_000;
/// 闪烁触发的横幅：正在展示的（flash key → 这张横幅）。同一实例闪烁期间
/// 只有这一张；图标停止闪烁（或消失）时整条收起（见 `flash_tick`）。
static LIVE_FLASH_CARDS: LazyLock<Mutex<HashMap<String, ()>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// 各微信图标最后一次「亮→灭」的时刻（Dock 每轮 publish_flash 调 `flash_seen`
/// 上报，存的是**灭帧时刻**而非上报时刻——Dock 在图标停闪后还会按自己的
/// 宽限窗口再报一会儿（同一个旧时刻），按上报时刻计会多等好几秒）
static FLASH_ALIVE: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// 距最后一次「亮→灭」超过这个时长 → 视为「不闪了」，对应横幅收起。
/// 微信亮→灭周期约 500ms，1.5s ≈ 3 个周期没动静才算停，停闪后横幅即收。
const FLASH_STOP_MS: u128 = 1_500;

struct Card {
    notice: BannerNotice,
    at: Instant,
    /// 这条的停留毫秒（None → 全局设置；0 = 常驻）
    ms: u128,
}

impl Card {
    /// 这张横幅对应的托盘闪烁 key（没有则 None）
    fn flash_key(&self) -> Option<&str> {
        self.notice.flash_key.as_deref()
    }
}

struct Banner {
    view: SoftBackend,
    /// 当前展示的横幅：最旧在前、最新在后；各自计时，到期独立移除
    cards: VecDeque<Card>,
    hovered: bool,
    mouse_tracked: bool,
    size: (i32, i32),
    /// 上次重排用的缩放（DPI × 横幅大小设置）：变了要整叠重排重绘
    last_scale: f32,
}

pub fn init(instance: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("ORBIT_WECARD"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let p = Box::into_raw(Box::new(Banner {
            view: SoftBackend::new(),
            cards: VecDeque::new(),
            hovered: false,
            mouse_tracked: false,
            size: (0, 0),
            last_scale: 0.0,
        }));
        match CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("ORBIT_WECARD"),
            PCWSTR::null(),
            WS_POPUP,
            0, 0, 10, 10,
            HWND::default(), HMENU::default(), instance,
            Some(p as *const core::ffi::c_void),
        ) {
            Ok(h) => {
                BANNER_HWND.store(h.0 as isize, Ordering::Relaxed);
                // 100ms：横幅过期；500ms：跟随设置项启停读取线程
                let _ = SetTimer(h, TIMER_CARD, 100, None);
                let _ = SetTimer(h, TIMER_CTRL, 500, None);
            }
            Err(_) => drop(Box::from_raw(p)),
        }
    }
}

fn hwnd() -> HWND {
    HWND(BANNER_HWND.load(Ordering::Relaxed) as *mut _)
}

/// 任意线程调用：按来源的通知通道分发——Windows 通知转给系统，Off 丢弃，
/// Orbit 横幅继续走自绘。每次弹出按 sound_mode 播提示音。
pub fn show(notice: BannerNotice) {
    use crate::settings::NotifyChannel;
    match settings::notify_channel_for(notice.source) {
        NotifyChannel::Off => return,
        NotifyChannel::Windows => {
            let title = if notice.badge.is_empty() {
                notice.tag.clone()
            } else {
                format!("{} · {}", notice.tag, notice.badge)
            };
            let body = if notice.sub.is_empty() {
                notice.main.clone()
            } else {
                format!("{}\n{}", notice.main, notice.sub)
            };
            crate::wintoast::show(&title, &body);
            if !notice.silent {
                crate::notify::play_banner();
            }
            return;
        }
        NotifyChannel::Orbit => {}
    }
    let h = hwnd();
    let raw = Box::into_raw(Box::new(notice));
    unsafe {
        if h.is_invalid()
            || PostMessageW(h, WM_BANNER, WPARAM(0), LPARAM(raw as isize)).is_err()
        {
            drop(Box::from_raw(raw)); // 窗口还没建好：别泄漏
        }
    }
}

/// 设置页「横幅大小」滑块的实时预览入口。
///
/// 设置页跑在**独立子进程**里，没有横幅窗口——所以这里只做一件事：按类名找到
/// 主进程的横幅窗口，把滑条值随消息带过去（`WM_BANNER_PREVIEW`），由主进程
/// 写入设置内存并弹预览卡。直接在本进程弹卡是弹不出来的（窗口根本不存在）。
pub fn show_preview() {
    let v = settings::banner_scale_setting();
    unsafe {
        let h = FindWindowW(w!("ORBIT_WECARD"), PCWSTR::null()).unwrap_or_default();
        if h.is_invalid() {
            return;
        }
        let _ = PostMessageW(
            h,
            crate::appmsg::WM_BANNER_PREVIEW,
            WPARAM(0),
            LPARAM(v.to_bits() as isize),
        );
    }
}

/// 设置页「手动拿 UIA」按钮：让读取线程立刻全量抓一次微信 UIA 树
pub fn request_grab() {
    if !settings::wechat_cards() {
        show(BannerNotice::info("微信 UIA", "请先开启「微信消息卡片」", "开关在 设置 → 模块与托盘"));
        return;
    }
    crate::wechatmsg::request_grab();
    wake(); // 读取线程若在等事件，立刻唤醒；它不在也没关系，标志会被下一轮读到
}

/// 供托盘闪帧调用：让读取线程立刻重读一轮
pub fn wake() {
    if let Ok(g) = READER.lock() {
        if let Some(r) = g.as_ref() {
            r.wake();
        }
    }
}

/// 托盘图标闪了一次（亮→灭）：去重后唤醒读取，让卡片跟提示音同步。
/// `at` 是这次闪烁的时刻（Dock 的 `flash_at`），同一帧重复调用只算一次。
pub fn on_flash(key: &str, at: Instant) {
    if !settings::wechat_cards() {
        return;
    }
    let fresh = match FLASH_SEEN.lock() {
        Ok(mut m) => {
            if m.get(key) == Some(&at) {
                false
            } else {
                m.insert(key.to_string(), at);
                true
            }
        }
        Err(_) => false,
    };
    if fresh {
        wake();
    }
}

/// 兜底卡片：托盘闪了一下但拿不到这个实例的 UIA 数据（微信 4.x 多开只有
/// 第一个实例能被读到会话列表），或者 UIA 卡片整体关着（默认模式）——
/// 发一条「有新消息」横幅顶上，点击按 PID 跳到对应微信窗口。
/// 闪烁轮次去重：同一轮连续闪烁（相邻两闪 <1s）只弹一张横幅、响一声，
/// 横幅按时长到期收起后图标还在闪也不重弹；停闪 ≥1s 再闪才算新一轮。
/// 图标不闪了由 `flash_tick` 收起。时长与提示音都和接管后的微信卡片
/// 走同一套设置（横幅显示时长 / sound_mode）。
pub fn on_flash_card(key: &str, at: Instant, pid: u32, name: &str) {
    let Ok(mut seen) = FLASH_CARD_SEEN.lock() else { return };
    if seen.get(key) == Some(&at) {
        return; // 同一帧重复调用
    }
    seen.insert(key.to_string(), at);
    // 闪烁轮次去重：同一轮连续闪烁（相邻两闪 <1s）只弹一张横幅、响一声。
    // 横幅按「横幅显示时长」到期收起后图标还在闪 → 不重弹不重响；
    // 停闪 ≥ 1 秒后再闪才算新一轮，才再弹再响（与接管后"一条消息一声"一致）
    let now = Instant::now();
    let same_burst = match FLASH_BURST_AT.lock() {
        Ok(mut m) => {
            let new_burst = match m.get(key) {
                Some(prev) => now.duration_since(*prev).as_millis() >= FLASH_BURST_GAP_MS,
                None => true,
            };
            m.insert(key.to_string(), now);
            !new_burst
        }
        Err(_) => false,
    };
    if same_burst {
        return;
    }
    // 该实例的横幅还在展示中（闪烁未停）：不重复弹
    if let Ok(live) = LIVE_FLASH_CARDS.lock() {
        if live.contains_key(key) {
            return;
        }
    }
    let account = if name.is_empty() { "微信" } else { name };
    let mut notice = BannerNotice::wechat(account, "有新消息", 0, pid);
    notice.flash_key = Some(key.to_string());
    notice.source = "tray"; // 托盘图标监管触发的兜底横幅，走托盘通知通道
    // 时长与接管后的微信卡片一致：不覆写，交给该来源的「横幅显示时长」设置
    // （托盘闪烁行；0 = 常驻）。没覆写时 accept() 按 banner_secs_for("tray") 取值。
    // 提示音与接管后一致：横幅自己按 wechat_sound 静音位 + sound_mode 播
    // （accept() 里 !silent → play_banner），不再由 Dock 侧按闪烁轮次单独响。
    notice.silent = !settings::wechat_sound();
    show(notice);
}

/// Dock 每轮发布闪烁快照时上报「这个微信图标还在闪」（`at` 是本次亮→灭的
/// 灭帧时刻）。横幅窗口的 500ms 定时器里 `flash_tick` 检查：距最后一次
/// 灭帧超过 `FLASH_STOP_MS` 就收横幅。
pub fn flash_seen(key: &str, at: Instant) {
    if let Ok(mut m) = FLASH_ALIVE.lock() {
        // 存灭帧时刻而不是上报时刻：停闪后 Dock 还会按宽限窗口再报一会儿
        // （同一个旧 at），按上报时刻计会让横幅多挂好几秒才收
        m.insert(key.to_string(), at);
    }
}

/// pid → (托盘图标 key, 最近一次「还在闪」的上报时刻)。Dock 每轮 publish_flash
/// 对正在闪烁的微信图标上报，多开各 pid 各自一条。UIA 微信卡片据此把横幅
/// 生命周期挂到托盘闪烁上：图标不闪了（消息已读）横幅随 flash_tick 收起——
/// 横幅秒数设 0（常驻）时不再永远挂着。
static FLASH_BY_PID: LazyLock<Mutex<HashMap<u32, (String, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Dock 上报：pid 的微信托盘图标还在闪（`key` 是图标稳定键，多开各一条；
/// `at` 是本次灭帧时刻，过期判定与 `flash_seen` 同一套）
pub fn flash_seen_pid(pid: u32, key: &str, at: Instant) {
    if pid == 0 || key.is_empty() {
        return;
    }
    if let Ok(mut m) = FLASH_BY_PID.lock() {
        // 顺手清理：实例退出后 pid 不再上报，60s 后清掉防堆积
        m.retain(|_, (_, t)| t.elapsed().as_millis() < 60_000);
        m.insert(pid, (key.to_string(), at));
    }
}

/// 该 pid 的托盘图标当前是否在闪（距最后一次「亮→灭」≤ `FLASH_STOP_MS`）：
/// 在闪返回图标 key，供微信卡片挂 flash_key（不闪了随 flash_tick 收起）。
/// 拿不到（托盘隐藏 / 刚启动还没见过闪烁）返回 None，卡片保持原行为。
pub fn flashing_key_for_pid(pid: u32) -> Option<String> {
    let m = FLASH_BY_PID.lock().ok()?;
    let (k, t) = m.get(&pid)?;
    (t.elapsed().as_millis() <= FLASH_STOP_MS).then(|| k.clone())
}

/// 收起已经不闪的微信横幅（在横幅窗口自己的定时器里跑，线程安全）。
/// 「不闪」= 距最后一次「亮→灭」（灭帧时刻）超过 `FLASH_STOP_MS`
/// （用户点开消息、图标消失、Dock 隐藏都会走到这里）。
pub fn flash_tick() {
    let expired: Vec<String> = match FLASH_ALIVE.lock() {
        Ok(m) => m
            .iter()
            .filter(|(_, t)| t.elapsed().as_millis() > FLASH_STOP_MS)
            .map(|(k, _)| k.clone())
            .collect(),
        Err(_) => return,
    };
    if expired.is_empty() {
        return;
    }
    if let Ok(mut m) = FLASH_ALIVE.lock() {
        for k in &expired {
            m.remove(k);
        }
    }
    let h = hwnd();
    for key in expired {
        let raw = Box::into_raw(Box::new(key));
        unsafe {
            if h.is_invalid()
                || PostMessageW(h, WM_BANNER_STOP, WPARAM(0), LPARAM(raw as isize)).is_err()
            {
                drop(Box::from_raw(raw));
            }
        }
    }
}

fn sync_reader() {
    // 读取线程有两项消费方：新消息卡片，以及 Dock 的悬停预览浮窗。
    // 任一开启就要跑——否则默认配置（卡片关、预览开）下快照永远空，浮窗弹不出来。
    let want = settings::wechat_cards() || settings::wechat_hover_preview();
    let Ok(mut g) = READER.lock() else { return };
    if want && g.is_none() {
        *g = Reader::start();
        crate::tlog!("[banner] 微信消息读取已启动");
    } else if !want && g.is_some() {
        *g = None; // Reader::drop 会置停止标志 + join
        unsafe { let _ = ShowWindow(hwnd(), SW_HIDE); }
        crate::tlog!("[banner] 微信消息读取已停止");
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CREATE {
            let cs = *(l.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(h, GWLP_USERDATA, cs.lpCreateParams as isize);
            return LRESULT(0);
        }
        let ptr = GetWindowLongPtrW(h, GWLP_USERDATA) as *mut Banner;
        if ptr.is_null() {
            return DefWindowProcW(h, msg, w, l);
        }
        let c = &mut *ptr;
        match msg {
            WM_BANNER => {
                if l.0 != 0 {
                    let notice = Box::from_raw(l.0 as *mut BannerNotice);
                    c.accept(h, *notice);
                }
                LRESULT(0)
            }
            // 设置页子进程发来的「横幅大小」预览请求（lParam = 滑条原始值 f32 bits）。
            // 子进程没有横幅窗口，预览必须由主进程画；先把缩放写进本进程的设置内存
            // （子进程的 preview 只改它自己的内存，主进程本来就看不到），再弹预览卡
            m if m == crate::appmsg::WM_BANNER_PREVIEW => {
                let v = f32::from_bits(l.0 as u32);
                if v.is_finite()
                    && v >= settings::BANNER_SCALE_MIN
                    && v <= settings::BANNER_SCALE_MAX
                {
                    settings::preview_banner_scale(v);
                    c.accept(
                        h,
                        BannerNotice {
                            tag: "预览".to_string(),
                            badge: format!("{:.0}%", (settings::banner_scale_setting() * 100.0).round()),
                            main: "横幅大小预览".to_string(),
                            sub: "拖动滑块调整大小，松手后几秒自动消失".to_string(),
                            silent: true,
                            pid: 0,
                            secs: Some(4),
                            flash_key: None,
                            source: "other",
                            preview: true,
                        },
                    );
                }
                LRESULT(0)
            }
            WM_BANNER_STOP => {
                // 这个托盘图标不闪了（或图标没了）：收起它对应的横幅
                if l.0 != 0 {
                    let key = Box::from_raw(l.0 as *mut String);
                    c.unregister(Some(&key));
                    let before = c.cards.len();
                    c.cards.retain(|card| card.flash_key() != Some(&key));
                    if c.cards.len() != before {
                        c.hovered = false;
                        if c.cards.is_empty() {
                            let _ = ShowWindow(h, SW_HIDE);
                            // "直到消息消失"提示音：横幅全没了就停
                            crate::notify::stop_until_dismiss();
                        } else {
                            c.place(h);
                            c.render(h);
                        }
                    }
                }
                LRESULT(0)
            }
            WM_TIMER => {
                if w.0 == TIMER_CTRL {
                    sync_reader();
                    flash_tick(); // 顺带检查：不闪了的微信横幅该收了
                    // 横幅大小设置变了（配置文件改写 / 外部通知）：正在显示的
                    // 整叠立即按新尺寸重排重绘（预览卡走 WM_BANNER 自己会重排）
                    let s = c.scale(h);
                    if (s - c.last_scale).abs() > 1e-4 {
                        c.last_scale = s;
                        if !c.cards.is_empty() {
                            c.hovered = false;
                            c.place(h);
                            c.render(h);
                        }
                    }
                } else if w.0 == TIMER_CARD && !c.cards.is_empty() {
                    // 每张按自己的到达时间与秒数独立到期；悬停时整叠暂停；0 秒 = 不自动关闭
                    let hovered = c.hovered;
                    let before = c.cards.len();
                    let mut dropped_keys: Vec<String> = Vec::new();
                    c.cards.retain(|card| {
                        let ms = if card.ms == 0 { u128::MAX } else { card.ms };
                        let keep = hovered || card.at.elapsed().as_millis() < ms;
                        if !keep {
                            if let Some(k) = card.flash_key() {
                                dropped_keys.push(k.to_string());
                            }
                        }
                        keep
                    });
                    for k in dropped_keys {
                        c.unregister(Some(&k));
                    }
                    if c.cards.len() != before {
                        c.hovered = false;
                        if c.cards.is_empty() {
                            let _ = ShowWindow(h, SW_HIDE);
                            // "直到消息消失"提示音：横幅全没了就停
                            crate::notify::stop_until_dismiss();
                        } else {
                            c.place(h);
                            c.render(h);
                        }
                    }
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                c.hovered = true;
                if !c.mouse_tracked {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: h,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    c.mouse_tracked = true;
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                c.mouse_tracked = false;
                c.hovered = false;
                let now = Instant::now();
                for card in c.cards.iter_mut() {
                    card.at = now; // 鼠标离开后整叠再给一个完整停留时间
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                // 点到哪一张就收起哪一张，其余保留；微信消息卡点一下跳到对应窗口
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                let scale = c.scale(h);
                let step = (CARD_H + CARD_GAP) * scale;
                let idx = (y as f32 / step) as usize;
                if let Some(card) = c.cards.remove(idx) {
                    c.unregister(card.flash_key());
                    if card.notice.pid != 0 {
                        // 微信消息卡片（source = wechat，main 就是会话名）：
                        // 开关开着就直达那个对话，否则只把微信窗口唤出来。
                        // 托盘闪烁的兜底横幅（source = tray，main 是账号名不是会话名）
                        // 没有可跳的对话，一直走「只唤出窗口」。
                        if settings::wechat_banner_jump()
                            && card.notice.source == "wechat"
                            && !card.notice.main.is_empty()
                        {
                            crate::tlog!(
                                "[banner] 点击卡片直达会话 pid={} chat={:?}",
                                card.notice.pid,
                                card.notice.main
                            );
                            crate::wechatmsg::open_session(
                                card.notice.pid,
                                card.notice.main.clone(),
                            );
                        } else {
                            crate::wechatmsg::focus(card.notice.pid);
                        }
                    }
                }
                c.hovered = false;
                if c.cards.is_empty() {
                    let _ = ShowWindow(h, SW_HIDE);
                } else {
                    c.place(h);
                    c.render(h);
                }
                LRESULT(0)
            }
            WM_NCHITTEST => {
                let pt = POINT {
                    x: (l.0 & 0xFFFF) as u16 as i16 as i32,
                    y: ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
                };
                let mut r = RECT::default();
                let _ = GetWindowRect(h, &mut r);
                let x = pt.x - r.left;
                let y = pt.y - r.top;
                let (ww, hh) = c.size;
                if inside_round(x as f32, y as f32, ww as f32, hh as f32, 21.0) {
                    LRESULT(HTCLIENT as isize)
                } else {
                    LRESULT(HTTRANSPARENT as isize)
                }
            }
            _ => DefWindowProcW(h, msg, w, l),
        }
    }
}

fn inside_round(x: f32, y: f32, w: f32, h: f32, r: f32) -> bool {
    if x < 0.0 || x > w || y < 0.0 || y > h {
        return false;
    }
    let r = r.min(w / 2.0).min(h / 2.0);
    let cx = if x < r { r } else if x > w - r { w - r } else { return true };
    let cy = if y < r { r } else if y > h - r { h - r } else { return true };
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

impl Banner {
    /// 接收一条横幅：响提示音、登记闪烁、预览卡原位替换，然后入队重排显示。
    /// `WM_BANNER`（真实横幅）与 `WM_BANNER_PREVIEW`（设置页预览）共用。
    fn accept(&mut self, h: HWND, notice: BannerNotice) {
        // 每次弹出都播一次提示音（微信卡片按 wechat_sound 设置可静音）
        if !notice.silent {
            crate::notify::play_banner();
        }
        // 闪烁触发的横幅登记为「展示中」，闪烁持续期间不再重复弹
        if let Some(key) = notice.flash_key.as_ref() {
            if let Ok(mut live) = LIVE_FLASH_CARDS.lock() {
                live.insert(key.clone(), ());
            }
        }
        // 预览卡：先摘掉旧的预览卡再入队——滑块拖动时原位换新尺寸，
        // 不会越拖堆得越高
        if notice.preview {
            self.cards.retain(|card| !card.notice.preview);
        }
        // 新横幅追加到队尾（屏幕下方），旧的留在上面，不替换当前显示内容。
        // 停留秒数：模块自己给的优先，否则用该来源的「横幅秒数」（0 = 常驻）
        let secs = notice
            .secs
            .unwrap_or_else(|| settings::banner_secs_for(notice.source));
        self.cards.push_back(Card {
            notice,
            at: Instant::now(),
            ms: (secs as u128).saturating_mul(1000),
        });
        while self.cards.len() > MAX_CARDS {
            if let Some(dropped) = self.cards.pop_front() {
                self.unregister(dropped.flash_key());
            }
        }
        self.hovered = false;
        self.place(h);
        self.render(h);
        let _ = unsafe { ShowWindow(h, SW_SHOWNOACTIVATE) };
    }

    /// 从「展示中」登记里摘掉一条闪烁横幅（横幅被收起/点掉/到期时调用）
    fn unregister(&self, key: Option<&str>) {
        if let Some(k) = key {
            if let Ok(mut live) = LIVE_FLASH_CARDS.lock() {
                live.remove(k);
            }
        }
    }

    fn scale(&self, h: HWND) -> f32 {
        let dpi = unsafe { GetDpiForWindow(h) } as f32 / 96.0;
        (settings::banner_scale() * dpi).max(0.6)
    }

    fn place(&mut self, h: HWND) {
        let scale = self.scale(h);
        let n = self.cards.len().max(1);
        let w = (CARD_W * scale).round() as i32;
        let one = (CARD_H * scale).round() as i32;
        let gap = (CARD_GAP * scale).round() as i32;
        // 整叠高度：最新一条贴在底部（靠 Dock），旧的往上递增
        let hh = one * n as i32 + gap * (n as i32 - 1);
        self.size = (w, hh);
        let mon = crate::window::primary_monitor();
        // Dock 在屏幕底部：横幅抬到 Dock 上方，别盖住图标
        let dock_h = if cfg::SHOW_DOCK && settings::dock_anchor() == settings::DockAnchor::Bottom {
            (cfg::DOCK_H * settings::icon_scale()).round() as i32 + cfg::DOCK_BOTTOM_MARGIN + 48
        } else {
            48
        };
        // 距右缘多留一些：整体比原来向左挪一点，不顶着屏幕边
        let x = (mon.rcMonitor.right - w - 64).max(mon.rcMonitor.left + 8);
        let y = (mon.rcMonitor.bottom - dock_h - hh).max(mon.rcMonitor.top + 8);
        unsafe { let _ = SetWindowPos(h, HWND_TOPMOST, x, y, w, hh, SWP_NOACTIVATE); }
    }

    fn render(&mut self, h: HWND) {
        let (w, hh) = self.size;
        if w <= 0 || hh <= 0 || self.cards.is_empty() {
            return;
        }
        let scale = self.scale(h);
        let notices: Vec<BannerNotice> = self.cards.iter().map(|c| c.notice.clone()).collect();
        let rgba = paint(&mut self.view, w, hh, scale, &notices);
        let bgra = render::premultiplied_rgba_to_bgra(&rgba);
        render::present(h, &bgra, w, hh);
    }
}

fn txt(p: &egui::Painter, x: f32, y: f32, s: &str, size: f32, color: u32) {
    p.text(
        egui::pos2(x, y),
        egui::Align2::LEFT_TOP,
        s,
        egui::FontId::proportional(size),
        argb(color),
    );
}

fn paint(soft: &mut SoftBackend, w: i32, h: i32, scale: f32, notices: &[BannerNotice]) -> Vec<u8> {
    soft.set_scale(scale);
    let fw = w as f32 / scale;
    let fh = h as f32 / scale;
    let ctx = soft.ctx().clone();
    let input = soft.input(fw, fh);
    let mut out = ctx.run_ui(input, |ui| {
        let p = ui.painter().clone();
        let t = cfg::cur();
        let pad = 24.0;
        let step = CARD_H + CARD_GAP;
        for (i, n) in notices.iter().enumerate() {
            // 最旧在最上面，最新在最下面（靠 Dock）
            let top = i as f32 * step;
            let board = egui::Rect::from_min_size(egui::pos2(0.0, top), egui::vec2(fw, CARD_H));
            glass_board(&p, board, egui::CornerRadius::same(21));
            p.rect_filled(
                egui::Rect::from_min_size(egui::pos2(0.0, top + 21.0), egui::vec2(4.5, CARD_H - 42.0)),
                egui::CornerRadius::same(3),
                argb(t.accent),
            );
            // 顶行：左侧标签 + 右侧小字
            if !n.tag.is_empty() {
                txt(&p, pad, top + 20.0, &n.tag, 16.5, t.accent);
            }
            if !n.badge.is_empty() {
                let sw = soft_text_width(soft, &n.badge, 16.5);
                txt(&p, fw - pad - sw, top + 20.0, &n.badge, 16.5, t.text_dim);
            }
            // 主行 + 副行
            let main = ellipsize(soft, &n.main, fw - pad * 2.0 - 6.0, 22.5);
            txt(&p, pad, top + 50.0, &main, 22.5, t.text);
            if !n.sub.is_empty() {
                let sub = ellipsize(soft, &n.sub, fw - pad * 2.0 - 6.0, 19.0);
                txt(&p, pad, top + 90.0, &sub, 19.0, t.text_dim);
            }
        }
    });
    soft.update_textures(&mut out.textures_delta);
    soft.rasterize(out.shapes, out.pixels_per_point, w, h)
}

fn soft_text_width(soft: &SoftBackend, s: &str, size: f32) -> f32 {
    soft.text_width(s, egui::FontId::proportional(size))
}
