//! 原生任务栏隐藏/恢复（对齐 C# ManagedShell.ExplorerHelper）。
//!
//! 托盘已被 trayhost 完整接管（自建假 Shell_TrayWnd + TaskbarCreated），镜像和悬停
//! 都不再依赖 UIA / PrintWindow，所以这里可以像 C# 一样**真隐藏**任务栏：
//!   - SHAppBarMessage(ABM_SETSTATE, ABS_AUTOHIDE)：让系统进入自动隐藏，工作区放开
//!   - SetWindowPos(真任务栏, HWND_BOTTOM, SWP_HIDEWINDOW | 不动位置大小)
//!   - 100ms watchdog：explorer 重启/换主题把它显示回来时再次隐藏
//! 退出（含 panic）时执行 restore_all：还原原始 ABM 状态 + SWP_SHOWWINDOW。
//!
//! 注意：trayhost 自建的假 Shell_TrayWnd 也是这个类名，隐藏时必须排除，否则会把
//! 我们自己的托盘宿主一起藏掉。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows::core::w;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::SetWinEventHook;
use windows::Win32::UI::Shell::{
    SHAppBarMessage, APPBARDATA, ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, EVENT_OBJECT_SHOW, FindWindowExW, GetClassNameW, GetWindowLongPtrW,
    IsWindowVisible, SetWindowPos, ShowWindow, SystemParametersInfoW,
    GWL_EXSTYLE, HWND_BOTTOM, HWND_TOP, LWA_ALPHA, SPI_GETWORKAREA, SPI_SETWORKAREA,
    SW_HIDE, SW_SHOW, SWP_FRAMECHANGED, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WINEVENT_OUTOFCONTEXT, WS_EX_LAYERED,
};

/// OBJID_WINDOW 的值
const OBJID_WINDOW: i32 = 0;

/// win_event_cb 重入保护：SHAppBarMessage(ABM_SETSTATE) / SetWindowPos(SWP_HIDEWINDOW)
/// 会让 explorer 再次显示任务栏，进而重入 EVENT_OBJECT_SHOW 回调。没有这个标志会无限
/// 递归直到栈溢出（退出广播 TaskbarCreated 时尤其容易触发）。
static IN_EVENT_CB: AtomicBool = AtomicBool::new(false);

static FOUND: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// 被隐藏过的真任务栏 hwnd（用于恢复可见性）
static HIDDEN: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// 是否处于隐藏状态（定时器/watchdog 用它决定要不要补一次）
static ACTIVE: Mutex<bool> = Mutex::new(false);
/// 启动时的 ABM 状态（退出时还原；None 表示没记录到）
static ORIG_STATE: Mutex<Option<usize>> = Mutex::new(None);
/// 我们改之前的工作区（退出时还原）
static ORIG_WORK_AREA: Mutex<Option<RECT>> = Mutex::new(None);
/// 隐藏前每个真任务栏的原始矩形 hwnd -> (left,top,right,bottom)，退出时显式移回。
/// ABM_SETSTATE(0) 只改 autohide 状态，不会把被 autohide 滑出屏幕的窗口移回来，
/// 必须显式 SetWindowPos 复原（对齐 hidecheck 的 `back` 命令）。
static ORIG_RECTS: Mutex<Vec<(usize, (i32, i32, i32, i32))>> = Mutex::new(Vec::new());

/// 当前是否隐藏着任务栏
pub fn is_active() -> bool {
    *ACTIVE.lock().unwrap()
}

fn abd(h: HWND) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: h,
        ..Default::default()
    }
}

pub fn hide_all() {
    let mut hidden = HIDDEN.lock().unwrap();
    for_each_tray(|h| unsafe {
        // 记录原始 ABM 状态（只记一次）
        if ORIG_STATE.lock().unwrap().is_none() {
            let st = SHAppBarMessage(ABM_GETSTATE, &mut abd(h));
            *ORIG_STATE.lock().unwrap() = Some(st);
            crate::tlog!("[hide] hide_all: hwnd={:#x} 原始 ABM state={st:#x}", h.0 as usize);
        }
        // 记录原始矩形（在 autohide 把它滑出屏幕之前）
        let mut rc = RECT::default();
        if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(h, &mut rc).is_ok() {
            let pair = (h.0 as usize, (rc.left, rc.top, rc.right, rc.bottom));
            let mut rects = ORIG_RECTS.lock().unwrap();
            if !rects.iter().any(|x| x.0 == pair.0) {
                rects.push(pair);
            }
        }
        // 自动隐藏 + 压到底层并隐藏（C# DoHideTaskbar）
        let mut d = abd(h);
        d.lParam = LPARAM(ABS_AUTOHIDE as isize);
        let _ = SHAppBarMessage(ABM_SETSTATE, &mut d);
        let _ = SetWindowPos(
            h, HWND_BOTTOM, 0, 0, 0, 0,
            SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        // 兼容兜底：某些主题 SetWindowPos 不生效时 ShowWindow 再补一次
        if IsWindowVisible(h).as_bool() {
            let _ = ShowWindow(h, SW_HIDE);
        }
        if !hidden.iter().any(|&x| x == h.0 as usize) {
            hidden.push(h.0 as usize);
        }
    });
    // 副屏任务栏（Shell_SecondaryTrayWnd）同样处理
    hide_secondary(&mut hidden);
    *ACTIVE.lock().unwrap() = true;
    release_work_area();
}

/// 把工作区放开到整块屏幕：ABM_SETSTATE(AUTOHIDE) 通常已经放开，这里兜底。
fn release_work_area() {
    unsafe {
        let mut wa = RECT::default();
        if SystemParametersInfoW(
            SPI_GETWORKAREA, 0,
            Some(&mut wa as *mut _ as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        ).is_err() {
            return;
        }
        let mon = crate::window::primary_monitor().rcMonitor;
        if wa.bottom >= mon.bottom {
            return; // 已经是整屏了
        }
        if ORIG_WORK_AREA.lock().unwrap().is_none() {
            *ORIG_WORK_AREA.lock().unwrap() = Some(wa);
        }
        let full = mon;
        let _ = SystemParametersInfoW(
            SPI_SETWORKAREA, 0,
            Some(&full as *const _ as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
}

/// 100ms 守护：explorer 重启/换主题会把任务栏显示回来，补一次（C# TaskbarMonitor_Tick）。
pub fn ensure_hidden() {
    if !is_active() {
        return;
    }
    let mut hidden = HIDDEN.lock().unwrap();
    for_each_tray(|h| unsafe {
        // 只读检查 + 按需重新隐藏
        if IsWindowVisible(h).as_bool()
            || SHAppBarMessage(ABM_GETSTATE, &mut abd(h)) & (ABS_AUTOHIDE as usize) == 0
        {
            let mut d = abd(h);
            d.lParam = LPARAM(ABS_AUTOHIDE as isize);
            let _ = SHAppBarMessage(ABM_SETSTATE, &mut d);
            let _ = SetWindowPos(
                h, HWND_BOTTOM, 0, 0, 0, 0,
                SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if !hidden.iter().any(|&x| x == h.0 as usize) {
            hidden.push(h.0 as usize);
        }
    });
    hide_secondary(&mut hidden);
    release_work_area();
}

/// 单次恢复动作：工作区 + ABM 状态 + 样式 + 位置 + 可见性。
unsafe fn restore_once(
    orig: &[usize],
    state: Option<usize>,
    rects: &[(usize, (i32, i32, i32, i32))],
    wa: Option<&RECT>,
) {
    if let Some(wa) = wa {
        let _ = SystemParametersInfoW(
            SPI_SETWORKAREA, 0,
            Some(wa as *const _ as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    for &h in orig {
        let hwnd = HWND(h as *mut _);
        // 还原原始 ABM 状态（autohide）
        if let Some(st) = state {
            let mut d = abd(hwnd);
            d.lParam = LPARAM(st as isize);
            let _ = SHAppBarMessage(ABM_SETSTATE, &mut d);
        }
        // 清掉可能被覆盖的分层样式（老版本 alpha 隐藏遗留）
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if ex & WS_EX_LAYERED.0 as isize != 0 {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetLayeredWindowAttributes(
                hwnd, windows::Win32::Foundation::COLORREF(0), 255, LWA_ALPHA,
            );
            let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                hwnd, GWL_EXSTYLE, ex & !(WS_EX_LAYERED.0 as isize),
            );
        }
        // 显式移回隐藏前的原始位置。ABM_SETSTATE(0) 只清 autohide 标志，
        // 不会把 autohide 滑出屏幕的窗口移回来（实测停在 y=1078），必须显式 SetWindowPos。
        if let Some((_, (l, t, r, b))) = rects.iter().find(|x| x.0 == h) {
            let _ = SetWindowPos(
                hwnd, HWND_TOP, *l, *t, *r - *l, *b - *t,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
}

/// 检查任务栏是否已真正恢复：无 autohide、可见、位置回到原始矩形。
unsafe fn restored(orig: &[usize], rects: &[(usize, (i32, i32, i32, i32))]) -> bool {
    orig.iter().all(|&h| {
        let hwnd = HWND(h as *mut _);
        let st = SHAppBarMessage(ABM_GETSTATE, &mut abd(hwnd));
        let visible = IsWindowVisible(hwnd).as_bool();
        let mut r = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut r);
        let want_top = rects.iter().find(|x| x.0 == h).map(|x| x.1.1).unwrap_or(1032);
        st & (ABS_AUTOHIDE as usize) == 0 && visible && (r.top - want_top).abs() <= 2
    })
}

pub fn restore_all() {
    *ACTIVE.lock().unwrap() = false;
    let wa = ORIG_WORK_AREA.lock().unwrap().take();
    let orig = std::mem::take(&mut *HIDDEN.lock().unwrap());
    let state = ORIG_STATE.lock().unwrap().take();
    let rects = std::mem::take(&mut *ORIG_RECTS.lock().unwrap());
    crate::tlog!("[hide] restore_all: hidden_n={} state={:?}", orig.len(), state);
    if orig.is_empty() {
        return;
    }
    unsafe {
        restore_once(&orig, state, &rects, wa.as_ref());
        // stop() 广播的 TaskbarCreated 由 explorer 异步处理，会在恢复之后把任务栏
        // 重新设为 autohide 并滑出屏幕（实测 ~150ms 内发生）。这里带验证地补恢复：
        // 每次间隔 150ms 检查，若被覆盖就再恢复一次，直到稳定或最多 8 次（1.2s）。
        for i in 0..8 {
            std::thread::sleep(std::time::Duration::from_millis(150));
            if restored(&orig, &rects) {
                crate::tlog!("[hide] restore 稳定（第 {i} 次检查）");
                return;
            }
            crate::tlog!("[hide] restore 被 explorer 覆盖，补恢复 #{i}");
            restore_once(&orig, state, &rects, wa.as_ref());
        }
        crate::tlog!("[hide] restore 最终状态: {:?}",
            { let mut r = RECT::default();
              let hwnd = HWND(orig[0] as *mut _);
              let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut r);
              (IsWindowVisible(hwnd).as_bool(), r.top) });
    }
}

/// 强制还原（不依赖隐藏时的记录）：清分层样式 + ABM 默认 + SWP_SHOWWINDOW。
/// 用于进程被强杀后手动恢复任务栏，也作为 `--restore-taskbar` 的实现。
pub fn force_restore() {
    *ACTIVE.lock().unwrap() = false;
    HIDDEN.lock().unwrap().clear();
    ORIG_STATE.lock().unwrap().take();
    // 工作区恢复到系统默认：用显示器工作区（去掉任务栏占位交给 explorer 自己算）
    for_each_tray(|h| unsafe {
        let mut d = abd(h);
        d.lParam = LPARAM(0); // ABS_DEFAULT
        let _ = SHAppBarMessage(ABM_SETSTATE, &mut d);
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        if ex & WS_EX_LAYERED.0 as isize != 0 {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetLayeredWindowAttributes(
                h, windows::Win32::Foundation::COLORREF(0), 255, LWA_ALPHA,
            );
            let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                h, GWL_EXSTYLE, ex & !(WS_EX_LAYERED.0 as isize),
            );
        }
        let _ = ShowWindow(h, SW_SHOW);
    });
}

/// 隐藏副屏任务栏（对齐 C# SetSecondaryTaskbarVisibility）。
fn hide_secondary(hidden: &mut Vec<usize>) {
    unsafe {
        let mut cur = HWND::default();
        loop {
            let Ok(n) = FindWindowExW(
                HWND::default(), cur, w!("Shell_SecondaryTrayWnd"), None,
            ) else { break };
            if crate::trayhost::is_own_hwnd(n) {
                cur = n;
                continue;
            }
            if IsWindowVisible(n).as_bool() {
                let _ = SetWindowPos(
                    n, HWND_BOTTOM, 0, 0, 0, 0,
                    SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            if !hidden.iter().any(|&x| x == n.0 as usize) {
                hidden.push(n.0 as usize);
            }
            cur = n;
        }
    }
}

/// 安装 WinEventHook：任何任务栏收到显示事件就立刻再隐藏。
/// 返回的钩子句柄进程生命周期内有效，无需显式注销。
pub fn install_hook() {
    unsafe {
        let hmod = GetModuleHandleW(None).unwrap_or_default();
        SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_SHOW,
            hmod,
            Some(win_event_cb),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
    }
}

/// 枚举全部真 Shell_TrayWnd（排除 trayhost 的假窗口；Win11 多屏每屏一个）并执行 f。
fn for_each_tray(mut f: impl FnMut(HWND)) {
    unsafe {
        FOUND.lock().unwrap().clear();
        let _ = EnumWindows(Some(enum_cb), LPARAM(0));
        let found: Vec<usize> = FOUND.lock().unwrap().clone();
        for h in found {
            let hwnd = HWND(h as *mut _);
            if crate::trayhost::is_own_hwnd(hwnd) {
                continue; // 别把托盘托管的假宿主一起藏了
            }
            f(hwnd);
        }
    }
}

unsafe extern "system" fn enum_cb(hwnd: HWND, _l: LPARAM) -> BOOL {
    let mut buf = [0u16; 64];
    let len = GetClassNameW(hwnd, &mut buf);
    let name = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
    if name == "Shell_TrayWnd" {
        FOUND.lock().unwrap().push(hwnd.0 as usize);
    }
    true.into()
}

unsafe extern "system" fn win_event_cb(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK, event: u32, hwnd: HWND,
    id_object: i32, _id_child: i32, _thread: u32, _time: u32,
) {
    if event != EVENT_OBJECT_SHOW || id_object != OBJID_WINDOW || !is_active() {
        return;
    }
    // 重入保护：本回调里的 SetWindowPos(SWP_HIDEWINDOW) 可能让 explorer 再次发出 SHOW
    // 事件；用 CAS 抢占，重入的直接丢弃，避免无限递归栈溢出。
    if IN_EVENT_CB.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return;
    }
    let mut buf = [0u16; 64];
    let len = GetClassNameW(hwnd, &mut buf);
    let name = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
    if name == "Shell_TrayWnd" && !crate::trayhost::is_own_hwnd(hwnd) {
        // 注意：这里**不要**再调 SHAppBarMessage(ABM_SETSTATE)。它会让 explorer 再次
        // 显示任务栏并重入本回调（退出广播 TaskbarCreated 时触发过栈溢出）。autohide
        // 状态在 hide_all 里已经设好，100ms watchdog (ensure_hidden) 会补，hook 只负责
        // 立刻 SWP_HIDEWINDOW 把闪出来的任务栏按回去。
        let _ = SetWindowPos(
            hwnd, HWND_BOTTOM, 0, 0, 0, 0,
            SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    IN_EVENT_CB.store(false, Ordering::SeqCst);
}
