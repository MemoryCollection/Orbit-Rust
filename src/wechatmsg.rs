//! 微信消息源（只读 UIA，参照 Atrium 的 wechat_source.cpp）：从微信主窗口的
//! 聊天列表读会话名、未读数和最新消息摘要，未读数上升时投一条通用横幅给
//! 右下角窗口。不修改微信、不开无障碍开关、不注入任何东西。
//!
//! 与托盘接管是两条独立通路：trayhost 只认 Shell 的 NOTIFYICONDATA（知道"闪了"），
//! 这里读的是微信自己的列表（知道"谁发的、说了什么"）。两条互补。
//!
//! 线程：`start()` 起一条 MTA 线程（UIA 客户端跑在 MTA），`stop()` 置标志 +
//! SetEvent 唤醒并 join。托盘闪帧走 `wake()` 立刻重读；设置页「手动拿 UIA」
//! 走 `request_grab()` 全量抓一次（含多开的所有微信进程），成功后右下角提示一次。
//!
//! 关到托盘：窗口隐藏后 Qt 把行从 UIA 树里摘掉，但**窗口还在时抓到的元素引用仍可
//! BuildUpdatedCache 刷新**，所以存一份 `held`（带 pid，支持多开），树里找不到行时
//! 接着从 held 读。没有任何 held（微信从没显示过窗口）时就没法读会话名——
//! 这时用「手动拿 UIA」按钮：唤出微信窗口 → 点按钮抓一次即可。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use windows::core::{BSTR, Interface, VARIANT};
use windows::Win32::Foundation::{
    BOOL, CloseHandle, FILETIME, HANDLE, HWND, LPARAM, POINT, RECT, WPARAM,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetProcessTimes, OpenProcess, SetEvent, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationCondition,
    IUIAutomation2, IUIAutomationElement, TreeScope_Descendants, UIA_AutomationIdPropertyId,
    UIA_BoundingRectanglePropertyId, UIA_ClassNamePropertyId, UIA_NamePropertyId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsHungAppWindow, IsIconic, IsWindowVisible, PostMessageW,
    GetClientRect, SetWindowPos, ShowWindow, SW_MINIMIZE, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_SYSCOMMAND, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER,
};

use crate::banner::BannerNotice;
use crate::settings;

const CELL_CLASS: &str = "mmui::ChatSessionCell";

/// 「手动拿 UIA」请求标志：设置页按钮置位，读取线程下一轮全量抓取并消位
static GRAB_REQ: AtomicBool = AtomicBool::new(false);
/// 「正在点会话行」标志：这期间让读取线程先别进 UIA，避免两路请求同时压在
/// 微信 UI 线程上（唤出窗口那一刻读取线程可能还在跑，前台跳过还没来得及生效）
static OPENING: AtomicBool = AtomicBool::new(false);
/// 首次成功抓到 UIA 是否已在右下角提示过（避免后台轮询反复弹）
static GRAB_NOTIFIED: AtomicBool = AtomicBool::new(false);

/// 设置页「手动拿 UIA」按钮调用：请求一次全量抓取。
/// 唤醒由调用方（`banner::request_grab()`）负责，这里只管置标志。
pub fn request_grab() {
    GRAB_REQ.store(true, Ordering::SeqCst);
    crate::tlog!("[wechatmsg] 收到手动抓取 UIA 请求");
}

/// 各微信实例的 UIA 拿取状态：pid → 是否拿到（读到会话行，或仍有 held 缓存）。
/// 设置页据此显示「多开哪个拿了、哪个没拿」。微信退出后对应 pid 会被清掉。
static GRABBED: LazyLock<Mutex<HashMap<u32, bool>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// 各实例拿取状态快照（按 pid 排序），设置页显示用
pub fn grab_status() -> Vec<(u32, bool)> {
    GRABBED
        .lock()
        .map(|m| {
            let mut v: Vec<(u32, bool)> = m.iter().map(|(p, g)| (*p, *g)).collect();
            v.sort_unstable();
            v
        })
        .unwrap_or_default()
}

/// 该 pid 的 UIA 数据当前是否可用（这轮读到过行，或有存活的 held 缓存）。
/// Dock 托盘闪烁兜底用：卡片开着但这个实例拿不到数据时，弹「有新消息」兜底横幅
/// （横幅自己按 sound_mode 响提示音），否则该实例的新消息会完全没有提示。
pub fn pid_readable(pid: u32) -> bool {
    GRABBED
        .lock()
        .map(|m| m.get(&pid).copied().unwrap_or(false))
        .unwrap_or(false)
}

/// 本轮被跳过（主窗口在前台）的微信进程：用户自己正在看，读取暂停。
/// 读取线程每轮刷新，天气卡片据此把该实例显示成「监控暂停」。
static PAUSED: LazyLock<Mutex<HashSet<u32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// 该 pid 是否因在前台而暂停读取
pub fn pid_paused(pid: u32) -> bool {
    PAUSED.lock().map(|m| m.contains(&pid)).unwrap_or(false)
}

/// 读取线程每轮调用：整组替换前台暂停集合（空 = 没有暂停的实例）
fn set_paused(pids: &[u32]) {
    if let Ok(mut m) = PAUSED.lock() {
        m.clear();
        m.extend(pids.iter().copied());
    }
}

/// 进程创建时间（100ns FILETIME）；取不到返回 0
fn proc_start(pid: u32) -> u64 {
    unsafe {
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return 0;
        };
        let (mut c, mut e, mut k, mut u) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let ok = GetProcessTimes(p, &mut c, &mut e, &mut k, &mut u).is_ok();
        let _ = CloseHandle(p);
        if ok { ((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64 } else { 0 }
    }
}

/// 该 pid 在多开实例里的标签（「微信1」/「微信2」，按进程创建时间序）。
/// 单开（只有一个实例）返回 None——单开没必要标，横幅少一段噪音。
/// 横幅弹出频率低，不缓存；找 pid 排序每次最多几十个进程，开销可忽略。
pub fn instance_tag(pid: u32) -> Option<String> {
    let mut procs: Vec<(u32, u64)> = wechat_processes()
        .into_iter()
        .map(|p| (p, proc_start(p)))
        .collect();
    procs.sort_by_key(|(_, t)| *t);
    let idx = procs.iter().position(|(p, _)| *p == pid)?;
    (procs.len() > 1).then(|| format!("微信{}", idx + 1))
}

/// 单个微信实例的运行状态（天气卡片「微信监视」区用）
#[derive(Clone)]
pub struct InstStatus {
    /// 主窗口标题（= 账号名），空串时调用方退化用「微信N」
    pub title: String,
    /// 绑定的热键（如 `F1`）；未配置为空串
    pub hotkey: String,
    /// 热键是否注册成功（未配置热键时无意义）
    pub hotkey_ok: bool,
    /// UIA 树是否拿到（读到过会话行或仍有存活缓存）→ 能读未读数才能弹横幅
    pub uia: bool,
    /// 是否处于实时监视（消息卡片开关开着）
    pub monitoring: bool,
    /// 主窗口是否在前台（读取暂停中，退到后台自动恢复）
    pub paused: bool,
}

/// 各实例状态快照，按启动先后排列（与热键槽位一致）。
/// 每 3s 才被天气卡片调一次，不做缓存。
pub fn instance_status() -> Vec<InstStatus> {
    let monitoring = settings::wechat_cards();
    let mut procs: Vec<(u32, u64)> = wechat_processes()
        .into_iter()
        .map(|pid| (pid, proc_start(pid)))
        .collect();
    procs.sort_by_key(|(_, t)| *t);
    let hotkeys = settings::wechat_hotkeys();
    let hk = crate::hotkey::status();
    let grab = grab_status();
    let titled = instance_windows_titled();
    procs
        .iter()
        .enumerate()
        .map(|(i, (pid, _))| {
            let title = titled
                .iter()
                .find(|(p, _, _)| p == pid)
                .map(|(_, _, t)| t.clone())
                .unwrap_or_default();
            let (spec, _) = hotkeys
                .get(i)
                .map(|e| crate::hotkey::split_binding(e))
                .unwrap_or(("", None));
            let hotkey = if spec.is_empty() || parse_hotkey_unrecognized(spec) {
                String::new()
            } else {
                spec.to_string()
            };
            let hotkey_ok = hk.iter().any(|(slot, _, ok)| *slot == i && *ok);
            let uia = grab.iter().any(|(p, g)| p == pid && *g);
            let paused = pid_paused(*pid);
            InstStatus { title, hotkey, hotkey_ok, uia, monitoring, paused }
        })
        .collect()
}

/// 热键写法是否解析不了（写错了就不显示，免得误导）
fn parse_hotkey_unrecognized(spec: &str) -> bool {
    crate::hotkey::parse_for_check(spec).is_none()
}

/// 各微信实例 (pid, 绑定热键)：按进程创建先后对应热键列表下标（与热键动作
/// `toggle(i)` 的槽位一致）。未配置 / 写法不认识的热键为空串。
/// 设置页账号监控表用；每帧调，枚举进程 + GetProcessTimes 开销与 instance_status 相当。
pub fn pid_hotkeys() -> Vec<(u32, String)> {
    let procs: Vec<(u32, u64)> = wechat_processes()
        .into_iter()
        .map(|pid| (pid, proc_start(pid)))
        .collect();
    let hotkeys = settings::wechat_hotkeys();
    procs
        .iter()
        .enumerate()
        .map(|(i, (pid, _))| {
            let (spec, _) = hotkeys
                .get(i)
                .map(|e| crate::hotkey::split_binding(e))
                .unwrap_or(("", None));
            let hotkey = if spec.is_empty() || parse_hotkey_unrecognized(spec) {
                String::new()
            } else {
                spec.to_string()
            };
            (*pid, hotkey)
        })
        .collect()
}

#[derive(Clone, Default)]
struct Row {
    chat: String,
    summary: String,
    unread: i32,
    muted: bool,
    /// 所属微信进程 PID（多开时区分，横幅点击跳到对应窗口）
    pid: u32,
}

/// "17:06" / "10/27" / "昨天" / "星期三"：摘要里这些不是消息内容。
/// 注意纯数字不算时间（真实时间行必含 `:` 或 `/`，如 17:06、2026/11/20）——
/// 否则「3」「123」这种纯数字消息内容会被误当时间丢掉，摘要变空。
fn is_time(s: &str) -> bool {
    if s.is_empty() || s.chars().count() > 12 {
        return false;
    }
    if s == "昨天" || s.starts_with("星期") {
        return true;
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_digit() || c == ':' || c == '/' || c == '-')
}

fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

/// 一行 UIA Name：会话名；"[N条] 摘要" 或 "[N条]" + 摘要独占一行；时间；"消息免打扰"
fn parse(name: &str) -> Row {
    let mut r = Row::default();
    let mut lines = name.split('\n').map(trim);
    let Some(first) = lines.next() else { return r };
    r.chat = first.to_string();
    for l in lines {
        if l == "消息免打扰" {
            r.muted = true;
            continue;
        }
        let mut l = l;
        if l.starts_with('[') && r.unread == 0 && r.summary.is_empty() {
            if let Some(close) = l.find("条]") {
                // '[' 是 1 字节，"]" 之后的 "条]" 是 3+1 字节；用字符边界切
                if close > 1 {
                    let inner_end = l[1..].find("条]").map(|k| 1 + k).unwrap_or(close);
                    r.unread = l[1..inner_end].parse::<i32>().unwrap_or(0);
                    l = trim(&l[inner_end + "条]".len()..]);
                }
            }
        }
        if !l.is_empty() && r.summary.is_empty() && !is_time(l) {
            r.summary = l.to_string();
        }
    }
    r
}

fn bstr_string(b: BSTR) -> String {
    String::from_utf16_lossy(b.as_wide())
}

/// 微信顶层 Qt 窗口（主窗口显示 / 最小化 / 关到托盘时都在）
#[repr(C)]
struct EnumCtx {
    pid: u32,
    out: Vec<windows::Win32::Foundation::HWND>,
}

unsafe extern "system" fn enum_windows_cb(
    h: windows::Win32::Foundation::HWND,
    l: LPARAM,
) -> BOOL {
    let c = &mut *(l.0 as *mut EnumCtx);
    let mut p = 0u32;
    let _ = GetWindowThreadProcessId(h, Some(&mut p));
    let mut cls = [0u16; 96];
    GetClassNameW(h, &mut cls);
    let name = String::from_utf16_lossy(&cls);
    if p == c.pid && name.contains("QWindowIcon") {
        c.out.push(h);
    }
    BOOL(1)
}

fn wechat_windows(pid: u32) -> Vec<windows::Win32::Foundation::HWND> {
    // extern "system" 回调不能捕获闭包：pid + 输出 Vec 走 LPARAM 传进去
    let mut data = EnumCtx { pid, out: Vec::new() };
    unsafe {
        let _ = EnumWindows(
            Some(enum_windows_cb),
            LPARAM(&mut data as *mut _ as isize),
        );
    }
    data.out
}

// ---------- 热键动作：显示 ↔ 最小化（绝不 hide） ----------

/// 微信主窗口标题（Qt 顶层窗口里还有「设置」等辅助窗，不能混进实例顺序）
fn is_main_window(h: HWND) -> bool {
    let mut buf = [0u16; 128];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    if n <= 0 {
        return false;
    }
    String::from_utf16_lossy(&buf[..n as usize]).contains("微信")
}

/// 按「实例创建先后」排列的微信主窗口：(pid, hwnd, 窗口标题)。
/// 标题就是账号名（UIA 树根的名字），热键按名称绑定（`F1=吴东`）靠它匹配。
/// 顺序与热键列表下标（`pid_hotkeys()` 同一套进程排序）一致，热键槽位才对得上。
fn instance_windows_titled() -> Vec<(u32, HWND, String)> {
    let mut procs: Vec<(u32, u64)> = wechat_processes()
        .into_iter()
        .map(|pid| (pid, proc_start(pid)))
        .collect();
    procs.sort_by_key(|(_, t)| *t);
    let mut out = Vec::new();
    for (pid, _) in procs {
        let wins = wechat_windows(pid);
        let pick = wins
            .iter()
            .copied()
            .find(|h| is_main_window(*h))
            .or_else(|| wins.first().copied());
        if let Some(h) = pick {
            let mut buf = [0u16; 128];
            let n = unsafe { GetWindowTextW(h, &mut buf) };
            let title = if n > 0 {
                String::from_utf16_lossy(&buf[..n as usize])
            } else {
                String::new()
            };
            out.push((pid, h, title));
        }
    }
    out
}

/// 热键动作：可见 → 最小化；已最小化 → 还原并置前；已隐藏 → 点托盘唤出
/// （隐藏后只能靠微信自己的 show()，外部 ShowWindow 重建不了 UIA 树）。
/// 槽位按实例启动先后对应：热键列表第 1 个 = 先启动的微信。
pub fn toggle(index: usize) {
    let list = instance_windows_titled();
    let Some((pid, _, _)) = list.get(index).map(|(p, h, t)| (*p, *h, t.clone())) else {
        crate::tlog!("[wechatmsg] 热键：没有第 {} 个微信实例", index + 1);
        return;
    };
    toggle_pid(pid);
}

/// 按账号快照 id 触发（`data/wechat_accounts/<id>`）。账号此刻没登录就什么都不做——
/// 热键照旧注册着，等它下次登录时同一颗键还能用。
pub fn toggle_account(acc: &str) {
    match crate::wechatsnap::logged_in(acc) {
        Some(pid) => {
            crate::tlog!("[wechatmsg] 热键（账号 {acc}）→ pid={pid}");
            toggle_pid(pid);
        }
        None => crate::tlog!("[wechatmsg] 热键：账号 {acc} 当前没有运行中的微信"),
    }
}

/// 按窗口标题关键词触发（老语法 `F1=名称关键词`）
pub fn toggle_named(kw: &str) {
    match instance_windows_titled()
        .into_iter()
        .find(|(_, _, t)| t.contains(kw))
    {
        Some((pid, _, title)) => {
            crate::tlog!("[wechatmsg] 热键（标题含 {kw:?}）→ pid={pid} title={title:?}");
            toggle_pid(pid);
        }
        None => crate::tlog!("[wechatmsg] 热键：没有标题含「{kw}」的微信实例"),
    }
}

/// 微信主窗口（优先真正的 `mmui::MainWindow`，退而取第一个）
fn main_hwnd(pid: u32) -> Option<HWND> {
    let wins = wechat_windows(pid);
    wins.iter()
        .copied()
        .find(|h| is_main_window(*h))
        .or_else(|| wins.first().copied())
}

/// 对指定 pid 的微信执行「显示 ↔ 最小化」
pub fn toggle_pid(pid: u32) {
    let Some(h) = main_hwnd(pid) else {
        crate::tlog!("[wechatmsg] 热键：pid={pid} 找不到微信主窗口");
        return;
    };
    unsafe {
        if !IsWindowVisible(h).as_bool() {
            crate::tlog!("[wechatmsg] 热键：微信 pid={} 已隐藏，点托盘唤出", pid);
            crate::dock::click_tray_for_pid(pid);
        } else if IsIconic(h).as_bool() {
            crate::tlog!("[wechatmsg] 热键：微信 pid={} 还原并置前", pid);
            crate::window::activate_window(h);
        } else {
            // 只最小化：窗口仍可见，Qt 的部件树完整保留，会话/未读继续可读
            let _ = ShowWindow(h, SW_MINIMIZE);
            crate::tlog!("[wechatmsg] 热键：微信 pid={} 最小化", pid);
        }
    }
}

// ---------- 关闭按钮拦截：点「关闭」改成最小化 ----------

/// 微信标题栏「关闭」按钮的位置，按**相对窗口右上角**的偏移缓存：
/// 窗口移动/缩放都不用重算，DPI 变了下一轮读取会刷新。
#[derive(Clone, Copy)]
struct CloseBtn {
    hwnd: isize,
    w: i32,
    h: i32,
    /// 窗口右边到按钮左边的距离
    dx: i32,
    /// 窗口顶到按钮顶的距离
    dy: i32,
}

static CLOSE_BTNS: LazyLock<Mutex<Vec<CloseBtn>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// 读取循环里顺手刷新该窗口「关闭」按钮的相对位置（只在正常显示时取，最小化时矩形无意义）
fn refresh_close_btn(auto: &IUIAutomation, root: &IUIAutomationElement, hwnd: HWND) {
    if !settings::wechat_close_min() {
        return;
    }
    if unsafe { IsIconic(hwnd).as_bool() || !IsWindowVisible(hwnd).as_bool() } {
        return;
    }
    let by_cls = unsafe {
        auto.CreatePropertyCondition(UIA_ClassNamePropertyId, &VARIANT::from("mmui::XButton"))
    };
    let by_name = unsafe { auto.CreatePropertyCondition(UIA_NamePropertyId, &VARIANT::from("关闭")) };
    let (Ok(by_cls), Ok(by_name)) = (by_cls, by_name) else {
        return;
    };
    let Ok(cond) = (unsafe { auto.CreateAndCondition(&by_cls, &by_name) }) else {
        return;
    };
    let Ok(arr) = (unsafe { root.FindAll(TreeScope_Descendants, &cond) }) else {
        return;
    };
    if unsafe { arr.Length() }.unwrap_or(0) == 0 {
        return;
    }
    let Ok(el) = (unsafe { arr.GetElement(0) }) else {
        return;
    };
    let Ok(b) = (unsafe { el.CurrentBoundingRectangle() }) else {
        return;
    };
    let mut r = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut r) }.is_err() {
        return;
    }
    let w = b.right - b.left;
    let h = b.bottom - b.top;
    if w <= 0 || h <= 0 {
        return;
    }
    let dx = r.right - b.left;
    let dy = b.top - r.top;
    if let Ok(mut v) = CLOSE_BTNS.lock() {
        let key = hwnd.0 as isize;
        match v.iter_mut().find(|c| c.hwnd == key) {
            Some(c) => {
                c.w = w;
                c.h = h;
                c.dx = dx;
                c.dy = dy;
            }
            None => v.push(CloseBtn { hwnd: key, w, h, dx, dy }),
        }
        if v.len() > 16 {
            let keep = v.len() - 16;
            v.drain(..keep);
        }
    }
}

/// 鼠标钩子调用：落点是否命中**前台**微信窗口的「关闭」按钮。
/// 命中就替用户最小化并返回 true（钩子据此吞掉这次点击）。
/// 只做两次廉价的窗口调用，不做 UIA，钩子里不会卡。
pub fn hit_close_and_minimize(x: i32, y: i32) -> bool {
    if !settings::wechat_close_min() {
        return false;
    }
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return false;
    }
    let key = fg.0 as isize;
    let btn = CLOSE_BTNS
        .lock()
        .ok()
        .and_then(|v| v.iter().find(|c| c.hwnd == key).copied());
    let Some(btn) = btn else {
        return false;
    };
    let mut r = RECT::default();
    if unsafe { GetWindowRect(fg, &mut r) }.is_err() {
        return false;
    }
    let left = r.right - btn.dx;
    let top = r.top + btn.dy;
    if x < left || x >= left + btn.w || y < top || y >= top + btn.h {
        return false;
    }
    const SC_MINIMIZE: usize = 0xF020;
    unsafe {
        let _ = PostMessageW(fg, WM_SYSCOMMAND, windows::Win32::Foundation::WPARAM(SC_MINIMIZE), LPARAM(0));
    }
    crate::tlog!("[wechatmsg] 点「关闭」改为最小化 hwnd={:?}", fg);
    true
}

/// 供 Dock 悬停预览浮窗用的一条未读会话（读取线程每轮整组替换）
#[derive(Clone, Default)]
pub struct WxUnread {
    pub pid: u32,
    pub chat: String,
    pub summary: String,
    pub unread: i32,
    pub muted: bool,
}

/// 未读会话快照：Dock 线程在悬停时 clone 一份，不阻塞、不碰 UIA。
static SNAPSHOT: LazyLock<Mutex<Vec<WxUnread>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// 当前所有实例的未读会话（按列表顺序，最多前 8 条）。
/// 微信在前台时读取线程暂停，快照会变空——那时用户自己就在看微信，不需要预览。
pub fn unread_snapshot() -> Vec<WxUnread> {
    SNAPSHOT
        .lock()
        .map(|v| v.iter().filter(|r| r.unread > 0).take(8).cloned().collect())
        .unwrap_or_default()
}

/// 读取线程每轮末尾调用：整组替换快照
fn publish_snapshot(rows: &[(String, Row)]) {
    let out: Vec<WxUnread> = rows
        .iter()
        .filter(|(_, r)| r.unread > 0)
        .map(|(_, r)| WxUnread {
            pid: r.pid,
            chat: r.chat.clone(),
            summary: r.summary.clone(),
            unread: r.unread,
            muted: r.muted,
        })
        .take(32)
        .collect();
    // 保持微信列表自己的顺序（最新的在最前），跟用户在微信里看到的一致
    if let Ok(mut s) = SNAPSHOT.lock() {
        *s = out;
    }
}

/// 离开作用域时清 OPENING（中途 return 也要清）
struct OpenGuard;
impl Drop for OpenGuard {
    fn drop(&mut self) {
        OPENING.store(false, Ordering::SeqCst);
    }
}

/// 直达会话：唤出该 pid 的微信窗口，再按会话名**单击**会话行。
///
/// 只做单击。**绝不**走 `LegacyIAccessible.DoDefaultAction()`——微信把它声明成
/// "双击"，而双击会把会话弹成独立的小窗口；用户要的是单击切换右侧对话区。
/// 也不用 `InvokePattern` / `SelectionItemPattern`——实测两个都是"返回成功却不干活"
/// （Invoke 返回 S_OK，选中行一动不动），Qt 的 UIA 实现只做了一半。
///
/// 真正的单击走**后台消息**：会话行中心的屏幕坐标转成微信窗口的客户区坐标，
/// 直接 PostMessage 一组 `WM_MOUSEMOVE / WM_LBUTTONDOWN / WM_LBUTTONUP`。
/// 实测有效，而且**真实光标一动不动**、不抢焦点、窗口被遮挡也不受影响——比注入
/// 真实鼠标事件干净得多（那会让用户看到光标自己跳过去再跳回来）。
///
/// **UIA 调用要极度克制**：微信的 UIA provider 跑在它自己的 UI 线程上，每次跨进程
/// 调用都要排队等它应答。本项目早就实测过"微信在前前台时读 UIA 会卡"（所以读取线程
/// 专门跳过前台进程），而这条路径恰恰是先把窗口唤到前台再读——所以这里必须
/// **一次 `FindAllBuildCache` 拿完全部名字+坐标**（1 次 RPC），绝不在循环里逐行
/// `CurrentName()`/`GetCurrentPatternAs()`，点完更不能再回读（那时微信正在切换
/// 对话、加载记录，最忙，任何 UIA 请求都会把它的界面卡住几秒）。
///
/// 会话名读不到（UIA 里根本没有这一行）时什么也不做——调用方已经把窗口唤出来了，
/// 表现就是"只唤出微信窗口"。
///
/// 整个动作在短命线程里做（自己初始化 MTA + UIA），不占读取线程——
/// 读取线程在微信刚被唤到前台时会暂停，靠它会等不到。
pub fn open_session(pid: u32, chat: String) {
    if pid == 0 || chat.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("wechat-open".into())
        .spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            OPENING.store(true, Ordering::SeqCst);
            let _guard = OpenGuard;
            if !wake_window(pid) {
                crate::tlog!("[wechatmsg] 直达会话失败：pid={pid} 窗口唤不出");
                return;
            }
            let clicked = click_session(pid, &chat);
            crate::tlog!("[wechatmsg] 直达会话 pid={pid} chat={chat:?} 单击={clicked}");
            CoUninitialize();
        });
}

/// 唤出微信窗口：可见就置前，收在托盘就点它的托盘图标。
/// 返回窗口是否已可见（唤不出就别继续点会话了）。
unsafe fn wake_window(pid: u32) -> bool {
    let vis = wechat_windows(pid)
        .into_iter()
        .find(|h| IsWindowVisible(*h).as_bool());
    if let Some(h) = vis {
        crate::window::activate_window(h);
        // 给 Qt 一点时间完成还原/重排，否则紧接着抓到的元素位置可能还是旧的
        std::thread::sleep(std::time::Duration::from_millis(220));
        return true;
    }
    crate::dock::click_tray_for_pid(pid);
    for _ in 0..25 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(h) = wechat_windows(pid)
            .into_iter()
            .find(|w| IsWindowVisible(*w).as_bool())
        {
            crate::window::activate_window(h);
            std::thread::sleep(std::time::Duration::from_millis(320));
            return true;
        }
    }
    false
}

/// 在可见的微信窗口里按会话名找到那一行，向它的中心投递一次后台单击（不动光标）。
///
/// 全程只对微信发起 **1 次** UIA 调用（`FindAllBuildCache`）：名字和坐标都在缓存里，
/// 循环里读的是本地缓存，不会再有跨进程 RPC。找不到行就直接返回 false（=只唤窗口）。
unsafe fn click_session(pid: u32, chat: &str) -> bool {
    let auto: IUIAutomation = match CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
        Ok(a) => a,
        Err(_) => return false,
    };
    // 兜底超时：微信 UI 线程忙时，UIA 默认会无限等下去，那会把我们和微信一起挂住
    if let Ok(a2) = auto.cast::<IUIAutomation2>() {
        let _ = a2.SetConnectionTimeout(1500);
        let _ = a2.SetTransactionTimeout(1500);
    }
    let cond = match auto.CreatePropertyCondition(
        UIA_ClassNamePropertyId,
        &VARIANT::from(CELL_CLASS),
    ) {
        Ok(c) => c,
        Err(_) => return false,
    };
    // 批量缓存：Name + BoundingRectangle 一次带回，且不建立完整元素引用
    // （AutomationElementMode_None），后续读属性全是本地数据
    let cache = match auto.CreateCacheRequest() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let _ = cache.AddProperty(UIA_NamePropertyId);
    let _ = cache.AddProperty(UIA_BoundingRectanglePropertyId);

    for h in wechat_windows(pid) {
        if !IsWindowVisible(h).as_bool() {
            continue;
        }
        // 用与读取线程完全一致的取法（ElementFromHandleBuildCache + FindAllBuildCache），
        // 确保缓存属性真的被填上；这样点前后总共只对微信发起 1 次批量请求
        let Ok(root) = auto.ElementFromHandleBuildCache(h, Some(&cache)) else {
            continue;
        };
        let Ok(all) = root.FindAllBuildCache(TreeScope_Descendants, &cond, &cache) else {
            continue;
        };
        let n = all.Length().unwrap_or(0);
        for i in 0..n {
            let Ok(e) = all.GetElement(i) else { continue };
            // 会话名的第一行才是名字，后面是 "[N条] 摘要 / 时间 / 消息免打扰"
            let name = e.CachedName().map(|b| b.to_string()).unwrap_or_default();
            let first = name.split('\n').next().unwrap_or("").trim();
            if first != chat {
                continue;
            }
            let r = e.CachedBoundingRectangle().unwrap_or_default();
            if r.right <= r.left || r.bottom <= r.top {
                continue;
            }
            let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
            // 换算到客户区坐标，并确认真的落在客户区内（缓存 rect 可能略过期）。
            // 不走 WindowFromPoint 校验——后台窗口常被别的窗口遮挡，那里命中的是
            // 遮挡者，会把正常情况误判成"不属于微信"。PostMessage 的目标窗口是明确的。
            let mut pt = POINT { x: cx, y: cy };
            if !windows::Win32::Graphics::Gdi::ScreenToClient(h, &mut pt).as_bool() {
                continue;
            }
            let mut rc = RECT::default();
            if GetClientRect(h, &mut rc).is_err() {
                continue;
            }
            if pt.x < 0 || pt.y < 0 || pt.x >= rc.right || pt.y >= rc.bottom {
                crate::tlog!("[wechatmsg] 会话行坐标 ({cx},{cy}) 不在微信客户区内，放弃");
                continue;
            }
            // 点完立刻收工：不再碰微信的 UIA 树（这时它正在切换对话，最忙）
            return click_point(h, pt);
        }
    }
    false
}

/// 在屏幕坐标上真实单击一次：移过去、按下抬起，点完把鼠标还给用户。
/// 后台单击：向窗口客户区的 `pt` 投递一组鼠标消息。
///
/// 真实光标一动不动 —— 这是关键：用 `SendInput`/`mouse_event` 注入真实事件会把
/// 光标拖到会话行再拖回来，用户能明显看到鼠标自己乱跑，还会抢焦点。
/// 实测这组 PostMessage 微信的 Qt 界面完全按单击处理（切换右侧对话、不弹独立窗口）。
unsafe fn click_point(h: HWND, pt: POINT) -> bool {
    // 客户区坐标打包进 lParam：低 16 位 x、高 16 位 y
    let lp = LPARAM(((pt.y as isize) << 16) | (pt.x as isize & 0xFFFF));
    let a = PostMessageW(h, WM_MOUSEMOVE, WPARAM(0), lp).is_ok();
    std::thread::sleep(std::time::Duration::from_millis(40));
    let b = PostMessageW(h, WM_LBUTTONDOWN, WPARAM(1), lp).is_ok();
    std::thread::sleep(std::time::Duration::from_millis(60));
    let c = PostMessageW(h, WM_LBUTTONUP, WPARAM(0), lp).is_ok();
    crate::tlog!("[wechatmsg] 后台单击会话行 客户区({},{})", pt.x, pt.y);
    a && b && c
}

/// 横幅点击跳转：把指定 PID 的微信窗口恢复并置前。多开时按 PID 找到
/// 对应那一个窗口。
/// 绝不用 `AttachThreadInput` 抢前台（老实现就栽在这里）：它会把目标进程的
/// 输入队列挂到本线程上，本线程一退出（或 detach 失败没检查返回值），
/// 目标的鼠标/键盘输入就整个楔死——2026-11-21 实测：横幅跳微信后微信完全
/// 无法操作（点哪都没反应，消息泵活着但输入进不来），连整个系统的前台
/// 切换都被拖死。现改用 `window::activate_window` 的无接触方案。
///
/// 微信主窗口收在托盘（不可见）时 SetForegroundWindow 无效，退回
/// `dock::click_tray_for_pid`：模拟左键它的托盘图标，让微信自己把窗口
/// 弹出来——和应用真人点击托盘的效果完全一致。
///
/// 整个动作放到**短命线程**里做：微信正忙（UIA 正在轮询它）时窗口调用
/// 也可能短暂变慢——横幅窗口线程绝不等微信，点完横幅 Orbit 立刻恢复响应。
pub fn focus(pid: u32) {
    if pid == 0 {
        return;
    }
    let _ = std::thread::Builder::new().name("wechat-focus".into()).spawn(move || unsafe {
        let wins = wechat_windows(pid);
        // 只认**可见**的窗口；收在托盘的隐藏窗口置前是无效操作
        let vis = wins
            .iter()
            .copied()
            .find(|h| IsWindowVisible(*h).as_bool());
        if let Some(h) = vis {
            crate::window::activate_window(h);
            crate::tlog!("[wechatmsg] 点击横幅，置前微信窗口 pid={} hwnd={:?}", pid, h);
        } else {
            crate::dock::click_tray_for_pid(pid);
        }
    });
}

/// 「居中」按钮：把指定 pid 的微信主窗口唤醒并移到屏幕工作区正中
/// （上下、左右都居中，尺寸不变）。唤醒复用热键那套逻辑：
/// 收进托盘（隐藏）→ 点托盘图标唤出（等窗口出现，最多 2s）；
/// 最小化 → 还原置前；已可见直接挪。动作放短命线程，不卡设置页 / 横幅线程。
pub fn center(pid: u32) {
    if pid == 0 {
        return;
    }
    let _ = std::thread::Builder::new().name("wechat-center".into()).spawn(move || unsafe {
        // ① 唤醒：先按主窗口挑目标（多开时辅助窗不参乎）
        let wins = wechat_windows(pid);
        let picked = wins
            .iter()
            .copied()
            .find(|h| is_main_window(*h))
            .or_else(|| wins.first().copied());
        let Some(picked) = picked else {
            crate::tlog!("[wechatmsg] 居中：pid={pid} 枚举不到微信窗口");
            return;
        };
        // ② 唤醒后的目标窗口：托盘唤出后用实际出现的那个句柄居中
        let target;
        if !IsWindowVisible(picked).as_bool() {
            // 收进托盘：让微信自己把窗口弹出来，再等它可见
            crate::dock::click_tray_for_pid(pid);
            let mut shown = None;
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if let Some(v) = wechat_windows(pid)
                    .into_iter()
                    .find(|w| IsWindowVisible(*w).as_bool())
                {
                    shown = Some(v);
                    break;
                }
            }
            let Some(h) = shown else {
                crate::tlog!("[wechatmsg] 居中：pid={pid} 点托盘后 2s 内窗口没出现");
                return;
            };
            target = h;
        } else if IsIconic(picked).as_bool() {
            // 最小化：还原并置前（SetWindowPos 挪最小化窗口只会改它还原后的位置）
            crate::window::activate_window(picked);
            target = picked;
        } else {
            target = picked;
        }
        // ③ 居中：工作区正中（上下、左右），尺寸不变、不动 z 序
        let mut r = RECT::default();
        if GetWindowRect(target, &mut r).is_err() {
            return;
        }
        let w = r.right - r.left;
        let ht = r.bottom - r.top;
        if w <= 0 || ht <= 0 {
            return;
        }
        let mi = crate::window::primary_monitor();
        let rc = if mi.rcWork.right > mi.rcWork.left && mi.rcWork.bottom > mi.rcWork.top {
            mi.rcWork
        } else {
            mi.rcMonitor
        };
        let x = rc.left + (rc.right - rc.left - w) / 2;
        let y = rc.top + (rc.bottom - rc.top - ht) / 2;
        let _ = SetWindowPos(target, HWND::default(), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        crate::tlog!("[wechatmsg] 居中微信窗口 pid={pid} → ({x},{y}) 尺寸 {w}x{ht}");
    });
}

fn wechat_processes() -> Vec<u32> {
    let mut out = Vec::new();
    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(s) => s,
            Err(_) => return out,
        };
        let mut pe = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut pe).is_ok();
        while ok {
            let name = String::from_utf16_lossy(&pe.szExeFile);
            let base = name.trim_end_matches('\0');
            if base.eq_ignore_ascii_case("Weixin.exe") || base.eq_ignore_ascii_case("WeChat.exe") {
                out.push(pe.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut pe).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    // 微信有一堆辅助子进程（更新/渲染/网络等），名字都叫 Weixin.exe。
    // 只保留 Dock 托盘里真实有图标的进程，其余的不枚举窗口、不进 UIA、
    // 也不出现在设置页的实例状态里；托管表没启动（未接管任务栏）时退回全列表。
    let hosted = crate::trayhost::hosted_pids();
    if hosted.is_empty() {
        out
    } else {
        out.into_iter().filter(|p| hosted.contains(p)).collect()
    }
}

/// 带 pid 前缀的键：多开时两个微信进程可能有同名会话，不能混在一个 baseline 里
fn row_key(pid: u32, id: &BSTR, chat: &str) -> String {
    let tail = if id.as_wide().is_empty() {
        chat.to_string()
    } else {
        bstr_string(id.clone())
    };
    format!("{pid}|{tail}")
}

/// 读循环每轮末尾刷新拿取状态表：进程退出后清掉对应条目。
/// 这轮读到过行、或仍有存活 held 缓存（刷新成功/前台跳过）的实例才算「拿到」。
fn update_grabbed(procs: &[u32], rows: &[(String, Row)], held_alive: &HashSet<u32>) {
    let Ok(mut m) = GRABBED.lock() else { return };
    m.retain(|pid, _| procs.contains(pid));
    for pid in procs {
        let got = rows.iter().any(|(_, r)| r.pid == *pid) || held_alive.contains(pid);
        m.insert(*pid, got);
    }
}

/// 白名单过滤日志：同一标题 30 秒只记一次，避免每轮扫描刷屏。
/// 用户靠 orbit.log 判断哪些标题被挡住了、要不要从白名单里删掉。
fn log_ignored(title: &str) {
    use std::sync::Mutex;
    static SEEN: LazyLock<Mutex<HashMap<String, u64>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
    let Ok(mut m) = SEEN.lock() else { return };
    let last = m.get(title).copied().unwrap_or(0);
    if now.saturating_sub(last) < 30_000 {
        return;
    }
    m.insert(title.to_string(), now);
    crate::tlog!("[wechatmsg] 白名单已跳过标题={}", title);
}

fn read_row(pid: u32, e: &IUIAutomationElement, rows: &mut Vec<(String, Row)>) {
    unsafe {
        let (Ok(id), Ok(name)) = (e.CachedAutomationId(), e.CachedName()) else { return };
        let mut r = parse(&bstr_string(name));
        if r.chat.is_empty() {
            return;
        }
        if settings::is_wechat_ignored(&r.chat) {
            log_ignored(&r.chat);
            return;
        }
        r.pid = pid;
        rows.push((row_key(pid, &id, &r.chat), r));
    }
}

/// 未读数上升（且非免打扰）→ 弹横幅；被用户读掉时更新计数，下次再涨才算新消息。
fn update_unread(
    rows: &[(String, Row)],
    baseline: bool,
    unread: &mut HashMap<String, i32>,
) {
    let mut rebuilt = HashMap::new();
    for (i, (key, r)) in rows.iter().enumerate() {
        let old = unread.get(key);
        let rose = baseline
            && !r.muted
            && match old {
                Some(n) => r.unread > *n,
                None => r.unread > 0 && i < 3,
            };
        if rose {
            // 标题和正文都写进 orbit.log：用户靠日志判断要把哪个标题加进白名单
            crate::tlog!(
                "[wechatmsg] 新消息 | 标题={} | 内容={} | 未读={} | pid_key={}",
                r.chat, r.summary, r.unread, key
            );
            // 只开了「悬停预览」、没开「消息卡片」时，读取线程只为浮窗供数，
            // 不额外弹卡片——新消息提醒仍走托盘闪烁那条通用横幅，避免两条都弹
            if !settings::wechat_cards() {
                if baseline {
                    unread.insert(key.clone(), r.unread);
                } else {
                    rebuilt.insert(key.clone(), r.unread);
                }
                continue;
            }
            let mut notice = BannerNotice::wechat(&r.chat, &r.summary, r.unread, r.pid);
            // 只有常驻卡片（横幅秒数=0）才挂到托盘闪烁上：图标不闪了（消息已读）
            // 随 flash_tick 收起。计时横幅**不能**挂——托盘图标一停闪它就被提前
            // 收起，表现为「横幅闪一下就没了」（2026-11-23 实测）。
            let eff_secs = notice
                .secs
                .unwrap_or_else(|| settings::banner_secs_for(notice.source));
            if eff_secs == 0 {
                if let Some(k) = crate::banner::flashing_key_for_pid(r.pid) {
                    notice.flash_key = Some(k);
                }
            }
            crate::banner::show(notice);
        }
        if baseline {
            unread.insert(key.clone(), r.unread);
        } else {
            rebuilt.insert(key.clone(), r.unread);
        }
    }
    if !baseline {
        *unread = rebuilt;
    }
    if unread.len() > 512 {
        unread.retain(|k, _| rows.iter().any(|(key, _)| key == k));
        while unread.len() > 512 {
            if let Some(k) = unread.keys().next().cloned() {
                unread.remove(&k);
            }
        }
    }
}

/// HANDLE 在 windows-rs 里不是 Send；这里只在同线程创建/关闭，用 newtype 明示
struct WakeHandle(HANDLE);
unsafe impl Send for WakeHandle {}
unsafe impl Sync for WakeHandle {}

pub struct Reader {
    stop: Arc<AtomicBool>,
    wake: WakeHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Reader {
    /// 起一条 MTA 线程读微信聊天列表；横幅由 `banner::show()` 全局投递
    pub fn start() -> Option<Self> {
        let wake = unsafe { CreateEventW(None, false, false, windows::core::PCWSTR::null()) }.ok()?;
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        // HANDLE 在 windows-rs 里不是 Send：过一下 isize，线程里再包回来
        let wake_raw = wake.0 as isize;
        let thread = std::thread::Builder::new()
            .name("wechat-msg".into())
            .spawn(move || unsafe { run(HANDLE(wake_raw as *mut _), s) })
            .ok();
        if thread.is_none() {
            unsafe { let _ = CloseHandle(wake); }
            return None;
        }
        Some(Self { stop, wake: WakeHandle(wake), thread })
    }

    /// 托盘闪帧时调用：立刻重读一轮，不等 2s 的兜底轮询
    pub fn wake(&self) {
        unsafe { let _ = SetEvent(self.wake.0); }
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        unsafe { let _ = SetEvent(self.wake.0); }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe { let _ = CloseHandle(self.wake.0); }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.finish();
        }
    }
}

unsafe fn run(wake: HANDLE, stop: Arc<AtomicBool>) {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

    let auto: IUIAutomation = match CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
        Ok(a) => a,
        Err(e) => {
            crate::tlog!("[wechatmsg] UIA 初始化失败 {:?}", e.code());
            unsafe { CoUninitialize() };
            return;
        }
    };

    let v = VARIANT::from(CELL_CLASS);
    let cells: IUIAutomationCondition = unsafe {
        auto.CreatePropertyCondition(UIA_ClassNamePropertyId, &v)
            .unwrap_or_else(|_| auto.CreateTrueCondition().unwrap())
    };
    let cache: IUIAutomationCacheRequest = match auto.CreateCacheRequest() {
        Ok(c) => c,
        Err(_) => {
            unsafe { CoUninitialize() };
            return;
        }
    };
    let _ = cache.AddProperty(UIA_AutomationIdPropertyId);
    let _ = cache.AddProperty(UIA_NamePropertyId);

    let mut unread: HashMap<String, i32> = HashMap::new();
    let mut baseline = false;
    let mut procs: Vec<u32> = Vec::new();
    let mut listed = 0u64;
    // 带 pid 的缓存元素：关到托盘后靠 BuildUpdatedCache 刷新，多开各自独立
    let mut held: Vec<(u32, IUIAutomationElement)> = Vec::new();
    let mut diag_at = 0u64;
    let mut last_row_count = 0usize;

    while !stop.load(Ordering::Relaxed) {
        if !settings::wechat_cards() {
            baseline = false;
            unread.clear();
            held.clear();
            set_paused(&[]);
            wait(wake, 2000);
            continue;
        }
        // 正在点会话行：这一轮什么都不读（不动 unread 基线，下次正常读时
        // 该算新消息的还会算），纯粹避免两路 UIA 请求同时压微信
        if OPENING.load(Ordering::Relaxed) {
            wait(wake, 300);
            continue;
        }
        let tick = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };

        // 手动抓取：全量刷新进程列表，并且这一次不受"微信在前台就不读"的限制
        let grab = GRAB_REQ.swap(false, Ordering::SeqCst);
        if grab || listed == 0 || tick - listed >= 10_000 {
            procs = wechat_processes();
            listed = tick;
        }

        // 前台微信进程不读：用户自己在看，每次按键都会触发 UIA，实测会卡。
        // 多开时只跳过前台那一个进程，其他微信实例照常读、照常弹横幅。
        let mut skipped: Vec<u32> = Vec::new();
        if !grab {
            let front = unsafe {
                let mut p = 0u32;
                let _ = GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut p));
                p
            };
            if procs.iter().any(|p| *p == front) {
                skipped.push(front);
            }
        }
        // 刷新「监控暂停」集合：天气卡片据此显示前台实例（手动抓取那轮不暂停）
        set_paused(&skipped);

        let mut rows: Vec<(String, Row)> = Vec::new();
        let mut seen = HashSet::new();
        let mut any_hung = false;
        let mut win_count = 0usize;
        let mut roots_ok = 0usize;
        let mut cells_total = 0usize;
        // 多开诊断：pid → (枚举到的窗口数, 读到的 Cell 数)
        let mut per_pid: HashMap<u32, (usize, usize)> = HashMap::new();
        let mut fresh_held: Vec<(u32, IUIAutomationElement)> = Vec::new();

        // 多开：遍历所有微信进程、每个进程的所有 Qt 窗口；不再找到第一个就 break。
        // 前台进程整组跳过（不 EnumWindows、不进 UIA），其余进程正常读。
        for pid in &procs {
            if skipped.contains(pid) {
                continue;
            }
            for h in wechat_windows(*pid) {
                win_count += 1;
                let pp = per_pid.entry(*pid).or_insert((0, 0));
                pp.0 += 1;
                if unsafe { IsHungAppWindow(h).as_bool() } {
                    any_hung = true;
                    continue;
                }
                let root = match auto.ElementFromHandleBuildCache(h, Some(&cache)) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                roots_ok += 1;
                // 顺手记下「关闭」按钮位置：点它时改成最小化（见 hit_close_and_minimize）
                refresh_close_btn(&auto, &root, h);
                let all = match root.FindAllBuildCache(
                    TreeScope_Descendants,
                    Some(&cells),
                    Some(&cache),
                ) {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                let n = all.Length().unwrap_or(0);
                cells_total += n as usize;
                pp.1 += n as usize;
                if n > 0 {
                    for i in 0..n {
                        if let Ok(e) = all.GetElement(i) {
                            read_row(*pid, &e, &mut rows);
                            fresh_held.push((*pid, e));
                        }
                    }
                }
            }
        }
        // 同一窗口/同一会话可能重复出现，按 key 去重（保留先到的元素引用）
        rows.retain(|(k, _)| seen.insert(k.clone()));
        let found = !rows.is_empty();
        if found {
            // 前台那一个微信这轮没读：保留它之前缓存的元素（关到托盘后还要续读），
            // 其他进程的换成这轮新抓的
            let kept: Vec<_> = held
                .iter()
                .filter(|(pid, _)| skipped.contains(pid))
                .cloned()
                .collect();
            held = fresh_held;
            held.extend(kept);
            if held.len() > 128 {
                held.drain(..held.len() - 128);
            }
        }

        // 关到托盘：树里没行了，用窗口还在时抓的元素刷新缓存
        let mut from_held = false;
        // 多开诊断：pid → (存活元素数, 失效元素数)
        let mut held_stat: HashMap<u32, (usize, usize)> = HashMap::new();
        if !found && !any_hung && !held.is_empty() && !procs.is_empty() {
            let mut fresh = Vec::with_capacity(held.len());
            for (pid, e) in held.drain(..) {
                if skipped.contains(&pid) {
                    // 前台进程这轮不读，元素原样保留，等它退到后台再刷；算存活
                    fresh.push((pid, e));
                    let st = held_stat.entry(pid).or_insert((0, 0));
                    st.0 += 1;
                    continue;
                }
                match e.BuildUpdatedCache(Some(&cache)) {
                    Ok(updated) => {
                        read_row(pid, &updated, &mut rows);
                        // 元素引用仍有效就保留——哪怕这轮没读出行（白名单/瞬时为空）。
                        // 以前"没读出行就丢"会在多开第二实例关到托盘的第一轮把它的
                        // held 整组清掉（其元素刷新失败或暂时读不出行），而 UIA 树里
                        // 此时也没有行可补 → 该实例托盘态永久失联、新消息永不提示。
                        fresh.push((pid, updated));
                        let st = held_stat.entry(pid).or_insert((0, 0));
                        st.0 += 1;
                    }
                    Err(_) => {
                        // 这轮刷新失败（窗口被销毁重建/微信重启）。也保留：
                        // 该实例窗口下次显示时 fresh_held 会整体换掉失效元素；
                        // 直接丢会让托盘态连「有缓存」的状态都查不到。
                        fresh.push((pid, e));
                        let st = held_stat.entry(pid).or_insert((0, 0));
                        st.1 += 1;
                    }
                }
            }
            held = fresh;
            // 进程已退出的实例不再留缓存
            held.retain(|(pid, _)| procs.contains(pid));
            let mut seen2 = HashSet::new();
            rows.retain(|(k, _)| seen2.insert(k.clone()));
            from_held = !rows.is_empty();
        }
        // held 续读也算"找到了会话行"：否则关到托盘后 found 恒为 false，
        // update_unread 永远不跑、baseline 永远 false → 新消息永不弹（13:48 实测回归）
        let found = !rows.is_empty();
        // 供 update_grabbed 判断「有缓存」：只有这轮还有存活元素的实例才算拿到
        let held_alive: HashSet<u32> = held_stat
            .iter()
            .filter(|(_, (ok, _))| *ok > 0)
            .map(|(pid, _)| *pid)
            .collect();

        // 刷新各实例的 UIA 拿取状态：读到会话行或仍有存活 held 缓存都算「拿到」，
        // 供设置页显示多开时哪个拿了、哪个没拿
        update_grabbed(&procs, &rows, &held_alive);

        // 诊断日志：UIA 到底有没有抓到会话行
        if !rows.is_empty() && rows.len() != last_row_count {
            let sample: Vec<(String, i32, String)> = rows.iter().take(3).map(|(_, r)| {
                (r.chat.clone(), r.unread, r.summary.clone())
            }).collect();
            crate::tlog!("[wechatmsg] 抓到 {} 行会话: {:?}", rows.len(), sample);
        }
        last_row_count = rows.len();
        if tick - diag_at >= 10_000 {
            diag_at = tick;
            crate::tlog!(
                "[wechatmsg] diag procs={} skip={} wins={} roots={} cells={} rows={} held={} held_read={} hung={} baseline={}",
                procs.len(), skipped.len(), win_count, roots_ok, cells_total,
                rows.len(), held.len(), from_held, any_hung, baseline
            );
            // 多开明细：每个实例枚举到几个窗口/几行 Cell、held 存活/失效各几个。
            // 排查"第二个微信拿不到"看这行：窗口 0 = 托盘态没有可枚举窗口，
            // held 全失效 = 元素随窗口销毁死了（需要窗口再显示一次才能补回来）。
            let wp: Vec<String> = per_pid
                .iter()
                .map(|(p, (w, c))| format!("{p}:w{w}c{c}"))
                .collect();
            let mut hp: Vec<String> = held_stat
                .iter()
                .map(|(p, (o, d))| format!("{p}:{o}+{d}"))
                .collect();
            hp.sort();
            crate::tlog!(
                "[wechatmsg] 多开明细 窗口/Cell: {} | held 存活+失效: {}",
                if wp.is_empty() { "-".into() } else { wp.join(" ") },
                if hp.is_empty() { "-".into() } else { hp.join(" ") }
            );
        }

        if grab {
            // 手动抓取：结果只提示一次；抓取本身不把现有未读当"新消息"弹出来
            if found {
                unread.clear();
                for (k, r) in &rows {
                    unread.insert(k.clone(), r.unread);
                }
                baseline = true;
                crate::banner::show(
                    BannerNotice::info(
                        "微信 UIA",
                        &format!("成功拿到（{} 个会话）", rows.len()),
                        "已缓存会话，关到托盘后也能弹卡片",
                    )
                    .with_secs(3),
                );
                GRAB_NOTIFIED.store(true, Ordering::Relaxed);
                crate::tlog!("[wechatmsg] 手动抓取成功: {} 行", rows.len());
            } else {
                baseline = false;
                crate::banner::show(
                    BannerNotice::info(
                        "微信 UIA",
                        "没拿到微信会话",
                        "请先显示一次微信主窗口，再点「手动拿 UIA」",
                    )
                    .with_secs(4),
                );
                crate::tlog!("[wechatmsg] 手动抓取失败：UIA 树里没有会话行");
            }
            wait(wake, 300);
            continue;
        }

        if procs.is_empty() {
            held.clear();
            unread.clear();
            baseline = false;
        } else if found {
            // 后台首次成功抓到时提示一次（手动抓过就不再重复提示）
            if !GRAB_NOTIFIED.swap(true, Ordering::Relaxed) {
                crate::banner::show(
                    BannerNotice::info(
                        "微信 UIA",
                        &format!("成功拿到（{} 个会话）", rows.len()),
                        "已缓存会话，关到托盘后也能弹卡片",
                    )
                    .with_secs(3),
                );
            }
            update_unread(&rows, baseline, &mut unread);
            baseline = true;
        } else {
            baseline = false;
        }
        // 悬停预览浮窗的数据源：每轮刷新（读不到就是一空组 → 浮窗不显示）
        publish_snapshot(&rows);

        wait(wake, 2000);
    }
    unsafe { CoUninitialize() };
}

unsafe fn wait(wake: HANDLE, ms: u32) {
    let _ = WaitForSingleObject(wake, ms);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chat_row() {
        let r = parse("张三\n[3条] 明天几点出发？\n17:06");
        assert_eq!(r.chat, "张三");
        assert_eq!(r.unread, 3);
        assert_eq!(r.summary, "明天几点出发？");
        assert!(!r.muted);
    }

    #[test]
    fn parses_muted_and_split_summary() {
        let r = parse("工作群\n[12条]\n收到，马上处理\n昨天\n消息免打扰");
        assert_eq!(r.chat, "工作群");
        assert_eq!(r.unread, 12);
        assert_eq!(r.summary, "收到，马上处理");
        assert!(r.muted);
    }

    #[test]
    fn ignores_time_as_summary() {
        let r = parse("文件传输助手\n2025/10/27");
        assert_eq!(r.summary, "");
        assert_eq!(r.unread, 0);
    }

    /// 纯数字的消息内容不是时间：不能把摘要丢掉（2026-11-23 实测「3」变空摘要）
    #[test]
    fn digit_only_summary_is_kept() {
        let r = parse("东\n[1条] \n3\n17:38");
        assert_eq!(r.unread, 1);
        assert_eq!(r.summary, "3");
        let r2 = parse("东\n[2条] \n123\n17:38");
        assert_eq!(r2.unread, 2);
        assert_eq!(r2.summary, "123");
    }
}
