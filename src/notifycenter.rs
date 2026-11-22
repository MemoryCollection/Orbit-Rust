//! 接管 Windows 通知中心（`UserNotificationListener`）。
//!
//! 开关在设置页「常规设置 → 通知方式 → 接管系统通知」（`settings::notify_takeover`）。
//! 打开后本模块的监听线程做三件事：
//!
//! 1. 在 **MTA 公寓**里拿 `UserNotificationListener::Current()`，并调一次
//!    `RequestAccessAsync()` 申请「通知监听」授权（系统级授权，只会问一次）。
//!    未打包的 exe 经常直接被拒（没有 MSIX 身份），此时把结果写进
//!    `data/notify-access.txt`，设置页读它显示「未授权」而不是假装成功。
//! 2. 授权通过后每 `POLL` 轮询一次 `GetNotificationsAsync(Toast)`，把**新出现**的通知
//!    （按 `Id` 去重）转成一条 `BannerNotice` 交给 `banner` 模块显示。
//! 3. 显示完立刻 `RemoveNotification(id)` 把它从通知中心撤掉 —— 这才是"接管"：
//!    同一条通知不会既在系统右下角弹一次、又在 Orbit 里弹一次。
//!
//! 首轮只登记已有通知的 id（不显示也不撤）：否则一开开关就被历史通知刷屏。
//! 关闭开关时线程只是待命（每 500ms 看一眼设置），不做任何 WinRT 调用。
//!
//! 线程模型与 `wintoast.rs` 一致：WinRT 调用放专用 MTA 线程，主线程是 STA
//! （`app.rs` 里 `CoInitializeEx(APARTMENTTHREADED)`），不能混用。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows::UI::Notifications::Management::{
    UserNotificationListener, UserNotificationListenerAccessStatus,
};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::banner::BannerNotice;
use crate::settings;

/// 有开关时的轮询间隔（通知列表很小，1.2s 足够及时又不烧 CPU）
const POLL: Duration = Duration::from_millis(1200);
/// 关闭（或授权失败）时的待命间隔
const IDLE: Duration = Duration::from_millis(500);
/// 授权被拒后的重试间隔：用户可能去系统设置里手动打开，别一直锤 WinRT
const RETRY: Duration = Duration::from_secs(10);
/// 去重表上限，超过就清空（撤过的通知不会再回来）
const SEEN_MAX: usize = 512;

/// 授权状态（写进 `data/notify-access.txt`，设置页子进程靠它显示结果）
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// 还没试过 / 开关没开
    Unknown,
    /// 已授权，正在接管
    Allowed,
    /// 被系统拒绝
    Denied,
}

fn access_path() -> std::path::PathBuf {
    crate::store::path("notify-access.txt")
}

/// 主进程写状态：第一行状态词，第二行给人看的原因
fn write_access(a: Access, detail: &str) {
    let word = match a {
        Access::Unknown => "unknown",
        Access::Allowed => "allowed",
        Access::Denied => "denied",
    };
    let _ = std::fs::write(access_path(), format!("{word}\n{detail}\n"));
}

/// 读状态（设置页子进程也调这个；进程内没有内存态，一律读文件）
pub fn access() -> Access {
    let text = std::fs::read_to_string(access_path()).unwrap_or_default();
    match text.lines().next().unwrap_or("").trim() {
        "allowed" => Access::Allowed,
        "denied" => Access::Denied,
        _ => Access::Unknown,
    }
}

/// 状态附带的原因（第二行，没有就是空串）
fn access_detail() -> String {
    std::fs::read_to_string(access_path())
        .unwrap_or_default()
        .lines()
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 设置页显示的一行说明（子进程读文件，主进程也读同一份，简单一致）
pub fn access_hint() -> String {
    match access() {
        Access::Allowed => "已授权：系统新通知会改在 Orbit 横幅里显示，并从通知中心撤掉".to_string(),
        Access::Denied => {
            let d = access_detail();
            if d.is_empty() {
                "系统未授权：通知监听需要应用身份（MSIX / 稀疏包），未打包的 exe 通常拿不到".to_string()
            } else {
                format!("系统未授权：{d}")
            }
        }
        Access::Unknown => "还没申请授权：勾上开关后系统会问一次（也可能直接拒绝）".to_string(),
    }
}

static STARTED: AtomicBool = AtomicBool::new(false);

/// 主进程启动时调一次：起监听线程（开关关闭时它只是待命）
pub fn start() {
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("orbit-notify".into())
        .spawn(worker);
    if spawned.is_err() {
        STARTED.store(false, Ordering::Relaxed);
        crate::tlog!("[notifycenter] 监听线程启动失败");
    }
}

fn worker() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    crate::tlog!("[notifycenter] 监听线程就绪（跟随设置项启停）");
    let mut listener: Option<UserNotificationListener> = None;
    let mut seen: HashSet<u32> = HashSet::new();
    // 首轮只登记已有通知，不弹也不撤
    let mut primed = false;
    let mut last_deny: Option<std::time::Instant> = None;
    loop {
        if !settings::notify_takeover() {
            if listener.take().is_some() {
                seen.clear();
                primed = false;
                write_access(Access::Unknown, "");
                crate::tlog!("[notifycenter] 开关已关闭，停止接管");
            }
            std::thread::sleep(IDLE);
            continue;
        }
        // 申请授权（失败后按 RETRY 间隔重试，不刷日志）
        if listener.is_none() {
            if let Some(t) = last_deny {
                if t.elapsed() < RETRY {
                    std::thread::sleep(IDLE);
                    continue;
                }
            }
            match open_listener() {
                Ok(l) => {
                    crate::tlog!("[notifycenter] 已获得通知监听授权，开始接管");
                    write_access(Access::Allowed, "");
                    listener = Some(l);
                    primed = false;
                    last_deny = None;
                }
                Err(e) => {
                    crate::tlog!("[notifycenter] 拿不到通知监听授权：{e}");
                    write_access(Access::Denied, &e);
                    last_deny = Some(std::time::Instant::now());
                    std::thread::sleep(IDLE);
                    continue;
                }
            }
        }
        let Some(l) = listener.as_ref() else { continue };
        if let Err(e) = pump(l, &mut seen, &mut primed) {
            // 监听对象失效（系统重启了通知服务 / 授权被收回）：扔掉重来
            crate::tlog!("[notifycenter] 轮询失败，重建监听：{e}");
            listener = None;
            seen.clear();
            primed = false;
            last_deny = Some(std::time::Instant::now());
        }
        std::thread::sleep(POLL);
    }
}

/// 取 listener 并申请授权；被拒时返回给人看的原因
fn open_listener() -> Result<UserNotificationListener, String> {
    let l = UserNotificationListener::Current()
        .map_err(|e| format!("通知监听不可用（{e}）"))?;
    let st = l
        .RequestAccessAsync()
        .and_then(|op| op.get())
        .map_err(|e| format!("申请授权失败（{e}）"))?;
    if st == UserNotificationListenerAccessStatus::Allowed {
        return Ok(l);
    }
    if st == UserNotificationListenerAccessStatus::Denied {
        return Err("系统拒绝了通知监听授权（未打包的 exe 没有 MSIX 身份，通常拿不到）".to_string());
    }
    Err("授权状态未确定（系统没有给出结论，稍后自动重试）".to_string())
}

/// 拉一次通知列表：新的显示 + 从通知中心撤掉
fn pump(
    l: &UserNotificationListener,
    seen: &mut HashSet<u32>,
    primed: &mut bool,
) -> windows::core::Result<()> {
    let list = l.GetNotificationsAsync(NotificationKinds::Toast)?.get()?;
    let size = list.Size()?;
    for i in 0..size {
        let Ok(n) = list.GetAt(i) else { continue };
        let Ok(id) = n.Id() else { continue };
        if !seen.insert(id) {
            continue; // 这一轮还是同一条
        }
        if !*primed {
            continue; // 首轮：只登记，不打扰
        }
        if let Some(notice) = notice_of(&n) {
            crate::banner::show(notice);
        }
        // 撤掉系统那条：同一条通知不要再在右下角弹第二次
        let _ = l.RemoveNotification(id);
    }
    if *primed && seen.len() > SEEN_MAX {
        seen.clear();
    }
    *primed = true;
    Ok(())
}

/// 把一条系统通知转成 Orbit 横幅：app 名当标签，第一行文本作主行，其余拼副行
fn notice_of(n: &UserNotification) -> Option<BannerNotice> {
    let app = n
        .AppInfo()
        .and_then(|a| a.DisplayInfo())
        .and_then(|d| d.DisplayName())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let texts = texts_of(n);
    if texts.is_empty() {
        return None;
    }
    let main = texts[0].clone();
    let sub = texts[1..].join(" · ");
    let tag = if app.is_empty() { "系统通知".to_string() } else { app };
    let mut notice = BannerNotice::info(&tag, &main, &sub);
    // 来源标记：`banner::show` 按来源选通道，未知来源 = Orbit 横幅
    notice.source = "system";
    // 原通知自己已经响过一次（是系统弹的），这里不再补提示音
    notice.silent = true;
    Some(notice)
}

/// 取通知正文：ToastGeneric 绑定里的全部文本行
fn texts_of(n: &UserNotification) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(notif) = n.Notification() else { return out };
    let Ok(visual) = notif.Visual() else { return out };
    let Ok(binding_name) = KnownNotificationBindings::ToastGeneric() else { return out };
    let Ok(binding) = visual.GetBinding(&binding_name) else { return out };
    let Ok(list) = binding.GetTextElements() else { return out };
    let Ok(size) = list.Size() else { return out };
    for i in 0..size {
        if let Ok(t) = list.GetAt(i).and_then(|t| t.Text()) {
            let s = t.to_string();
            if !s.trim().is_empty() {
                out.push(s.trim().to_string());
            }
        }
    }
    out
}
