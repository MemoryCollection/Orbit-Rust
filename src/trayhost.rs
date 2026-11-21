//! 托盘托管（对齐 C# Orbit / ManagedShell）：自建 `Shell_TrayWnd` + `TrayNotifyWnd`，
//! 广播 `TaskbarCreated` 让应用把托盘图标**重新注册到我们这里**。图标的
//! `hIcon`/tooltip/`uCallbackMessage`/`uVersion`/`guidItem` 全部从 `WM_COPYDATA` 的
//! `NOTIFYICONDATA` 直接得到——不再依赖 UIA，也就不怕隐藏真任务栏。
//!
//! 另外在 `WM_COPYDATA(dwData=3)` 里回答 `Shell_NotifyIconGetRect` 的位置查询：把我们
//! 在 Dock 上画的图标矩形报给应用，应用校验悬停坐标时就会认为指针落在"自己的"图标上。
//!
//! 与窗口线程的通信靠 `Arc<Mutex<State>>`：窗口过程在独立线程跑消息循环，Dock 线程读表。

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::core::{w, GUID, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetMessageW,
    GetSystemMetrics, GetWindowLongPtrW, IsWindow, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SendNotifyMessageW, SetWindowLongPtrW, SetWindowPos,
    TranslateMessage, GWL_STYLE, HWND_BROADCAST, HWND_TOPMOST, SM_CXSCREEN, WNDCLASSW,
    WM_COPYDATA, WM_DESTROY, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
};

/// 一个被我们接管的托盘图标（从 NOTIFYICONDATA 抄过来的裸字段）。
#[derive(Clone, Debug)]
pub struct Icon {
    pub hwnd: u32,
    pub uid: u32,
    pub guid: GUID,
    /// 唯一的注册回调消息号（WM_APP+x）
    pub msg: u32,
    /// 协议版本（0/3/4）
    pub ver: u32,
    /// 图标句柄（32 位，跨进程可绘制）；0 = 这一帧是"灭"（微信闪烁）
    pub hicon: u32,
    /// 当前这一帧是否为"灭"（NIF_ICON MODIFY 且 hicon=0）
    pub dim: bool,
    pub tip: String,
    pub flags: u32,
    pub state: u32,
    /// 图标所属进程 exe（小写全路径），从 hWnd 反查
    pub exe: String,
    pub pid: u32,
    /// 变更序号：每次 ADD/MODIFY 都 +1，Dock 用它决定是否重画像素
    pub seq: u64,
}

impl Icon {
    /// 两条注册是不是同一个图标：**两边都带 GUID** 才按 GUID 认，否则按 hWnd+uID。
    ///
    /// 对齐 Explorer 的匹配规则：一次调用带 `NIF_GUID` 才按 GUID 找，否则按 hWnd+uID。
    /// 以前"任一边有 GUID 就只比 GUID"会让 GUID 注册的图标错过**不带 GUID** 的
    /// NIM_MODIFY / NIM_SETVERSION——闪烁灭帧收不到，协议版本更新不上
    /// （v4 图标被当成 v0 编码消息，小米电脑管家实测点击行为错乱）。
    pub fn same(&self, o: &Icon) -> bool {
        if self.guid != GUID::default() && o.guid != GUID::default() {
            self.guid == o.guid
        } else {
            self.hwnd == o.hwnd && self.uid == o.uid
        }
    }
}

/// 我们上报给应用的"图标在哪"（Dock 里的屏幕矩形）。按 GUID 优先匹配。
#[derive(Clone, Copy)]
pub struct Placement {
    pub hwnd: u32,
    pub uid: u32,
    pub guid: GUID,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// 窗口线程 ↔ Dock 线程共享的表。
pub struct State {
    pub icons: Vec<Icon>,
    /// Dock 每次重画后写入自己的图标屏幕矩形，供 dwData=3 回答
    pub placements: Vec<Placement>,
    /// 图标表版本号，Dock 轮询时对比，变化才重建
    pub generation: u64,
}

impl Default for State {
    fn default() -> Self {
        Self { icons: Vec::new(), placements: Vec::new(), generation: 0 }
    }
}

pub struct Host {
    // 主 crate 内不读，但 `tools/trayhostcheck` 以 #[path] 引入本模块后使用
    #[allow(dead_code)]
    pub state: Arc<Mutex<State>>,
    hwnd: usize,
}

/// 全局句柄：WndProc 通过它拿到共享表；Host 退出后置空。
static TABLE: Mutex<Option<Arc<Mutex<State>>>> = Mutex::new(None);
static SEQ: AtomicU64 = AtomicU64::new(0);
/// 假 Shell_TrayWnd 的窗口过程要认得自己创建的窗口（避免抢到真任务栏的消息）
static OWN_HWND: Mutex<usize> = Mutex::new(0);
/// 假 TrayNotifyWnd（子窗口）——SetTrayHostSizeData 时一起移动
static NOTIFY_HWND: Mutex<usize> = Mutex::new(0);
/// 假托盘宿主矩形 (left, top, width, height)：Dock 每次重画后写入。
/// 对齐 C# SetTrayHostSizeData：把假窗口移动到这里，Shell_NotifyIconGetRect 才认这个宿主。
static HOST_RECT: Mutex<(i32, i32, i32, i32)> = Mutex::new((0, 0, 0, 0));
/// 守护线程退出标志
static GUARD_QUIT: AtomicBool = AtomicBool::new(false);
/// 是否允许把私有消息转发给真任务栏。stop() 广播 TaskbarCreated 前先关掉，
/// 否则 TaskbarCreated（注册消息，值 >= 0xC000）会被 SendMessage 转给真任务栏，
/// explorer 处理时又回发 → ping-pong 递归 → 栈溢出（已实测复现）。
static FORWARD_ENABLED: AtomicBool = AtomicBool::new(true);
/// forward_to_explorer 重入保护（SendMessage 是同步的，explorer 处理时会在本线程栈上
/// 回调 wndproc，形成递归；CAS 抢占可打断环）。
static FORWARDING: AtomicBool = AtomicBool::new(false);
/// 消息循环是否该退出
static QUITTING: Mutex<bool> = Mutex::new(false);

const WM_COPYDATA_APPBAR: usize = 0;
const WM_COPYDATA_TRAY: usize = 1;
const WM_COPYDATA_ICONRECT: usize = 3;

/// 启动托盘托管：创建假窗口 + 广播 TaskbarCreated + 起消息循环。
/// 必须在主线程之外调用（消息循环会占用调用线程直到 stop）。
pub fn start() -> Option<Host> {
    let state = Arc::new(Mutex::new(State::default()));
    *TABLE.lock().unwrap() = Some(state.clone());
    *QUITTING.lock().unwrap() = false;
    FORWARD_ENABLED.store(true, Ordering::SeqCst);
    FORWARDING.store(false, Ordering::SeqCst);
    GUARD_QUIT.store(false, Ordering::SeqCst);

    // WndProc 需要在独立线程的消息循环里收消息；用 channel 等窗口建好
    let (tx, rx) = std::sync::mpsc::channel::<usize>();
    let state_thread = state.clone();
    std::thread::spawn(move || unsafe { run(state_thread, tx) });
    let hwnd_val = rx.recv().ok()?;
    if hwnd_val == 0 {
        return None;
    }
    crate::tlog!("[trayhost] 假 Shell_TrayWnd 已建立 hwnd={hwnd_val:#x}，广播 TaskbarCreated");
    Some(Host { state, hwnd: hwnd_val })
}

unsafe fn run(_state: Arc<Mutex<State>>, tx: std::sync::mpsc::Sender<usize>) {
    let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
    let proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT = wndproc;

    let tray_cls = register_class(proc, "Shell_TrayWnd", hinst);
    let notify_cls = register_class(notify_wndproc, "TrayNotifyWnd", hinst);
    if tray_cls == 0 || notify_cls == 0 {
        let _ = tx.send(0);
        return;
    }

    // C# RegisterTrayWnd：全屏宽 × 23*dpi。尺寸后面由 SetTrayHostSize 改成 Dock 矩形，
    // 但初始就得是个"像托盘"的横条，别用 1×1——否则 Shell 的首轮位置查询会拿到的很奇怪。
    let cx = GetSystemMetrics(SM_CXSCREEN).max(1);
    let cy = 23;
    let htray = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        PCWSTR(tray_cls as *const u16),
        w!(""),
        WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
        0, 0, cx, cy,
        HWND::default(), None, hinst, None,
    );
    let Ok(htray) = htray else { let _ = tx.send(0); return };
    let hnotify = CreateWindowExW(
        windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
        PCWSTR(notify_cls as *const u16),
        PCWSTR::null(),
        WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
        0, 0, cx, cy, htray, None, hinst, None,
    );
    *OWN_HWND.lock().unwrap() = htray.0 as usize;
    if let Ok(n) = hnotify {
        *NOTIFY_HWND.lock().unwrap() = n.0 as usize;
    }
    let _ = tx.send(htray.0 as usize);

    // 守护线程（对齐 C# TrayMonitor_Tick）：确保 FindWindow("Shell_TrayWnd","")
    // 命中的是我们的假窗口而非真任务栏；否则图标注册和 GetRect 查询都会落到真任务栏。
    GUARD_QUIT.store(false, Ordering::Relaxed);
    std::thread::spawn(|| unsafe { guard_loop() });

    // 广播 TaskbarCreated：应用收到后会重新 NIM_ADD，消息发到我们这个假托盘
    let tbc = RegisterWindowMessageW(w!("TaskbarCreated"));
    if tbc != 0 {
        let _ = SendNotifyMessageW(
            HWND_BROADCAST,
            tbc, WPARAM(0), LPARAM(0),
        );
    }

    let mut msg = std::mem::zeroed();
    while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        let _ = DispatchMessageW(&msg);
        if *QUITTING.lock().unwrap() && msg.message == WM_DESTROY {
            PostQuitMessage(0);
        }
    }
}

/// 100ms 守护（对齐 C# TrayService.TrayMonitor_Tick + MakeTrayTopmost）：
/// 让假窗口始终排在真任务栏前面，这样应用重新注册图标、以及 Shell_NotifyIconGetRect
/// 的位置查询才会命中我们。同时把被系统意外加上的 WS_VISIBLE 抹掉（假托盘不可见）。
unsafe fn guard_loop() {
    while !GUARD_QUIT.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
        let own = *OWN_HWND.lock().unwrap();
        if own == 0 {
            continue;
        }
        let h = HWND(own as *mut c_void);
        // FindWindow 返回 Z 序最前的同名窗口；若不是我们，就把假窗口抬到 TOPMOST。
        let front = FindWindowW(w!("Shell_TrayWnd"), w!(""))
            .map(|x| x.0 as usize)
            .unwrap_or(0);
        if front != own {
            let _ = SetWindowPos(
                h, HWND_TOPMOST, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        // 假托盘保持隐藏：Win 偶尔会给顶层窗口补回 WS_VISIBLE。
        let style = GetWindowLongPtrW(h, GWL_STYLE);
        if style & WS_VISIBLE.0 as isize != 0 {
            let _ = SetWindowLongPtrW(h, GWL_STYLE, style & !(WS_VISIBLE.0 as isize));
        }
    }
}

unsafe fn register_class(
    proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    name: &str,
    hinst: HINSTANCE,
) -> u16 {
    let mut wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let wc = WNDCLASSW {
        style: windows::Win32::UI::WindowsAndMessaging::WNDCLASS_STYLES(0x8),
        lpfnWndProc: Some(proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
        hCursor: windows::Win32::UI::WindowsAndMessaging::HCURSOR::default(),
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(wide.as_mut_ptr()),
    };
    RegisterClassW(&wc)
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        return on_copydata(w, l);
    }
    if msg == WM_DESTROY {
        return LRESULT(0);
    }
    // 应用发的私有消息原样转给真任务栏（ManagedShell ForwardMsg 同样处理）
    if msg >= 0x0400 {
        forward_to_explorer(h, msg, w, l);
    }
    DefWindowProcW(h, msg, w, l)
}

unsafe extern "system" fn notify_wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        return on_copydata(w, l);
    }
    DefWindowProcW(h, msg, w, l)
}

/// 把不认识的消息转给真任务栏（`Shell_TrayWnd`，排除我们自己）。
///
/// 三重防护（都是被实测栈溢出逼出来的）：
///   1. 注册消息（>= 0xC000，含 TaskbarCreated）不转发——它不是应用私有托盘消息，
///      转给 explorer 会引发重新注册风暴。
///   2. stop() 期间 FORWARD_ENABLED=false，广播 TaskbarCreated 时完全不转发。
///   3. FORWARDING CAS 重入保护 + SendMessageTimeout，打断 explorer 回发造成的递归。
unsafe fn forward_to_explorer(_from: HWND, msg: u32, w: WPARAM, l: LPARAM) {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, SendMessageTimeoutW, SMTO_ABORTIFHUNG};
    if msg >= 0xC000 || !FORWARD_ENABLED.load(Ordering::SeqCst) {
        return; // 注册消息（TaskbarCreated 等）不该转发
    }
    if FORWARDING.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return;
    }
    let own = *OWN_HWND.lock().unwrap();
    let mut cur = HWND::default();
    loop {
        let Ok(n) = FindWindowExW(HWND::default(), cur, w!("Shell_TrayWnd"), PCWSTR::null()) else { break };
        if n.0 as usize == own {
            cur = n;
            continue;
        }
        let mut result: usize = 0;
        let _ = SendMessageTimeoutW(
            n, msg, w, l, SMTO_ABORTIFHUNG, 200, Some(&mut result),
        );
        break;
    }
    FORWARDING.store(false, Ordering::SeqCst);
}

unsafe fn on_copydata(_w: WPARAM, l: LPARAM) -> LRESULT {
    if l.0 == 0 {
        return LRESULT(0);
    }
    let cd = &*(l.0 as *const COPYDATASTRUCT);
    if cd.lpData.is_null() {
        return LRESULT(0);
    }
    match cd.dwData {
        WM_COPYDATA_TRAY => {
            if let Some(icon) = parse_shelltraydata(cd.lpData, cd.cbData as usize) {
                apply(icon);
                return LRESULT(1);
            }
        }
        WM_COPYDATA_ICONRECT => {
            return answer_icon_rect(cd.lpData, cd.cbData as usize);
        }
        WM_COPYDATA_APPBAR => {
            // AppBar 消息：转发给真任务栏，我们自己不注册 AppBar
        }
        _ => {}
    }
    LRESULT(0)
}

/// 解析 SHELLTRAYDATA：+0 magic, +4 dwMessage, +8 NOTIFYICONDATA(packed 956)。
/// 返回 (dwMessage, Icon)；NIM_DELETE 也走这里（hicon 可为 0）。
unsafe fn parse_shelltraydata(p: *mut c_void, cb: usize) -> Option<(u32, Icon)> {
    if cb < 8 + 0x3bc {
        return None;
    }
    let b = std::slice::from_raw_parts(p as *const u8, cb);
    let u32at = |off: usize| u32::from_le_bytes(b.get(off..off + 4).and_then(|x| x.try_into().ok()).unwrap_or([0; 4]));
    let dw_msg = u32at(4);
    let nid = 8usize;
    let cb_nid = u32at(nid) as usize;
    if cb_nid < 0x3b8 {
        return None;
    }
    // NOTIFYICONDATA 打包布局（句柄全 u32）：
    // +0 cbSize +4 hWnd +8 uID +12 uFlags +16 uCallbackMessage +20 hIcon
    // +24 szTip[128 wchar] +280 dwState +284 dwStateMask +288 szInfo[256]
    // +800 uVersion +804 szInfoTitle[64] +932 dwInfoFlags +936 guidItem(16) +952 hBalloonIcon
    let hwnd = u32at(nid + 4);
    let uid = u32at(nid + 8);
    let flags = u32at(nid + 12);
    let msg = u32at(nid + 16);
    let hicon = u32at(nid + 20);
    let state = u32at(nid + 280);
    let ver = u32at(nid + 800);
    let guid = GUID {
        data1: u32at(nid + 936),
        data2: u16::from_le_bytes(b[nid + 940..nid + 942].try_into().ok()?),
        data3: u16::from_le_bytes(b[nid + 942..nid + 944].try_into().ok()?),
        data4: b[nid + 944..nid + 952].try_into().ok()?,
    };
    let tip = wide_at(b, nid + 24, 128);
    let (pid, exe) = exe_of_hwnd(hwnd);
    let icon = Icon {
        hwnd, uid, guid, msg, ver, hicon, dim: false, tip, flags, state, exe, pid,
        seq: SEQ.fetch_add(1, Ordering::Relaxed),
    };
    Some((dw_msg, icon))
}

fn wide_at(b: &[u8], off: usize, max_wchar: usize) -> String {
    if off >= b.len() {
        return String::new();
    }
    let end = (off + max_wchar * 2).min(b.len());
    let mut v = Vec::new();
    for c in b[off..end].chunks_exact(2) {
        let u = u16::from_le_bytes([c[0], c[1]]);
        if u == 0 {
            break;
        }
        v.push(u);
    }
    String::from_utf16_lossy(&v).lines().next().unwrap_or("").trim().to_string()
}

/// 从图标宿主窗口反查 pid 与 exe（解决 tooltip 为空时认不出程序）。
fn exe_of_hwnd(hwnd: u32) -> (u32, String) {
    if hwnd == 0 {
        return (0, String::new());
    }
    unsafe {
        let h = HWND(hwnd as *mut c_void);
        let mut pid = 0u32;
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(h, Some(&mut pid));
        let exe = crate::tray::exe_of_pid(pid);
        (pid, exe)
    }
}

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETVERSION: u32 = 4;
const NIF_ICON: u32 = 0x2;

fn apply((dm, icon): (u32, Icon)) {
    let Some(tab) = TABLE.lock().unwrap().clone() else { return };
    let mut st = tab.lock().unwrap();
    // 每次入库前先清掉窗口已销毁的条目：任务管理器每次刷新 CPU 图表都会用新的
    // 通知窗口重复 ADD 一条「任务管理器」，旧窗口随后销毁但不发 NIM_DELETE，
    // 不清就会在 Dock 托盘里堆出十几个同名图标（实测 12 条）。
    prune_dead_locked(&mut st);
    match dm {
        NIM_DELETE => {
            st.icons.retain(|i| !i.same(&icon));
            st.generation += 1;
        }
        NIM_SETVERSION => {
            if let Some(i) = st.icons.iter_mut().find(|i| i.same(&icon)) {
                i.ver = icon.ver;
                i.seq = icon.seq;
                st.generation += 1;
            }
        }
        NIM_ADD | NIM_MODIFY => {
            // 任务管理器刷新 CPU 图表时用新窗口 ADD 一个 tip=「任务管理器」的占位图标，
            // 稳定后 MODIFY 成「CPU xx%」。占位不入库（用户要求最简策略），
            // 等它换成正式名字后走下面「未匹配 MODIFY 补入库」路径进来。
            let taskmgr_placeholder = icon.tip == "任务管理器";
            if taskmgr_placeholder {
                crate::tlog!(
                    "[trayhost] 跳过任务管理器占位图标 dm={} hwnd={:#x} uid={}",
                    dm, icon.hwnd, icon.uid
                );
            }
            // 微信类图标的每次变更都记一行，用来实测"空白-图标"闪烁走的是哪条通道
            let is_msg = icon.exe.contains("weixin") || icon.exe.contains("wechat") || icon.tip.contains("微信");
            if is_msg {
                crate::tlog!(
                    "[trayhost] MSG {} dm={} flags={:#x} hicon={:#x} state={:#x} tip={} hwnd={:#x} guid={:?}",
                    if dm == NIM_ADD { "ADD" } else { "MODIFY" }, dm, icon.flags, icon.hicon,
                    icon.state, icon.tip, icon.hwnd, icon.guid
                );
            }
            if let Some(i) = st.icons.iter_mut().find(|i| i.same(&icon)) {
                // MODIFY 只带 NIF_ICON 时才更新图标（hicon=0 = 灭帧，保留原图但记 dim）
                if icon.flags & NIF_ICON != 0 {
                    i.dim = icon.hicon == 0;
                    if icon.hicon != 0 {
                        i.hicon = icon.hicon;
                    }
                }
                if !icon.tip.is_empty() && !taskmgr_placeholder {
                    i.tip = icon.tip;
                }
                if icon.msg >= 0x0400 {
                    i.msg = icon.msg;
                }
                if icon.ver > 0 {
                    i.ver = icon.ver;
                }
                if icon.guid != GUID::default() {
                    i.guid = icon.guid;
                }
                i.flags |= icon.flags;
                i.state = icon.state;
                i.exe = icon.exe;
                i.pid = icon.pid;
                i.seq = icon.seq;
                st.generation += 1;
            } else if dm == NIM_ADD && !taskmgr_placeholder {
                let (tip, hwnd, uid, msg, ver, guid) =
                    (icon.tip.clone(), icon.hwnd, icon.uid, icon.msg, icon.ver, icon.guid);
                // 同一 exe 已经有一条（且没被 same() 匹配上）→ 记录 identity，
                // 用来区分"多开（不同 hwnd/guid）"和"幽灵重注册（窗口已换）"
                let dup = st.icons.iter().filter(|i| i.exe == icon.exe).count();
                if dup > 0 {
                    crate::tlog!(
                        "[trayhost] 重复 ADD exe={} tip={} hwnd={:#x} uid={} guid={:?} 已有{}条",
                        icon.exe, tip, hwnd, uid, guid, dup
                    );
                }
                st.icons.push(icon);
                st.generation += 1;
                crate::vlog!("[trayhost] ADD {} hwnd={:#x} uid={} msg={:#x} v{} guid={:?}",
                    tip, hwnd, uid, msg, ver, guid);
            } else if dm == NIM_MODIFY && !taskmgr_placeholder && !icon.tip.is_empty() {
                // 未匹配上的 MODIFY：ADD 被拦掉（任务管理器占位）或漏掉的图标，
                // 带着 tip 落库补进来。hicon/msg/ver/guid 都在 NOTIFYICONDATA 里，直接建。
                let (tip, hwnd, uid, msg, ver, guid) =
                    (icon.tip.clone(), icon.hwnd, icon.uid, icon.msg, icon.ver, icon.guid);
                st.icons.push(icon);
                st.generation += 1;
                crate::vlog!("[trayhost] MODIFY 补入库 {} hwnd={:#x} uid={} msg={:#x} v{} guid={:?}",
                    tip, hwnd, uid, msg, ver, guid);
            }
        }
        _ => {}
    }
}

/// hwnd 是否仍是一个活着的窗口（message-only 窗口也算活）
fn is_live(hwnd: u32) -> bool {
    hwnd != 0 && unsafe { IsWindow(HWND(hwnd as *mut c_void)).as_bool() }
}

/// 清掉托管表里窗口已销毁的条目（幽灵托盘图标）。
/// 任务管理器每次刷新 CPU 图表都会用新的通知窗口重新 ADD，旧窗口销毁时
/// 不发 NIM_DELETE，不清理会堆出十几个「任务管理器」图标，其中只有最后
/// 一条（tip 变成「CPU xx%」的那条）是活的。已持有表锁时用 _locked 版本。
fn prune_dead_locked(st: &mut State) {
    let before = st.icons.len();
    st.icons.retain(|i| i.hwnd == 0 || is_live(i.hwnd));
    let gone = before - st.icons.len();
    if gone > 0 {
        st.generation += 1;
        crate::tlog!("[trayhost] 清理失效图标 {} 条（通知窗口已销毁）", gone);
    }
}

/// Dock 轮询时调用（2s 一次）：没有新注册也能把幽灵图标清掉
pub fn prune_dead_icons() {
    let Some(tab) = TABLE.lock().unwrap().clone() else { return };
    let Ok(mut st) = tab.lock() else { return };
    prune_dead_locked(&mut st);
}

/// 回答 dwData=3：WINNOTIFYICONIDENTIFIER{dwMagic,dwMessage,cbSize,dwPadding,hWnd,uID,guidItem}
/// 返回 MAKELPARAM(left,top) 或 (right,bottom)。
unsafe fn answer_icon_rect(p: *mut c_void, cb: usize) -> LRESULT {
    crate::vlog!("[trayhost] GetRect 查询到达 cb={cb}");
    if cb < 40 {
        return LRESULT(0);
    }
    let b = std::slice::from_raw_parts(p as *const u8, cb);
    let u32at = |off: usize| u32::from_le_bytes(b.get(off..off + 4).and_then(|x| x.try_into().ok()).unwrap_or([0; 4]));
    let dm = u32at(4);
    let hwnd = u32at(16);
    let uid = u32at(20);
    let guid = GUID {
        data1: u32at(24),
        data2: u16::from_le_bytes([b.get(28).copied().unwrap_or(0), b.get(29).copied().unwrap_or(0)]),
        data3: u16::from_le_bytes([b.get(30).copied().unwrap_or(0), b.get(31).copied().unwrap_or(0)]),
        data4: b.get(32..40).and_then(|x| x.try_into().ok()).unwrap_or([0; 8]),
    };
    crate::vlog!(
        "[trayhost] GetRect dm={dm} hwnd={hwnd:#x} uid={uid} guid={guid:?} placements_n={}",
        TABLE.lock().unwrap().as_ref().map(|t| t.lock().unwrap().placements.len()).unwrap_or(0)
    );
    if dm != 1 && dm != 2 {
        return LRESULT(0);
    }
    let Some(tab) = TABLE.lock().unwrap().clone() else { return LRESULT(0) };
    let st = tab.lock().unwrap();
    let found = st.placements.iter().find(|p| {
        if guid != GUID::default() {
            p.guid == guid
        } else {
            p.hwnd == hwnd && p.uid == uid
        }
    });
    if let Some(pl) = found {
        let (x, y) = if dm == 1 { (pl.left, pl.top) } else { (pl.right, pl.bottom) };
        let packed = ((y as u16 as u32) << 16) | (x as u16 as u32);
        crate::vlog!("[trayhost] GetRect 命中 dm={dm} → ({x},{y}) packed={packed:#x}");
        return LRESULT(packed as isize);
    }
    crate::vlog!("[trayhost] GetRect 未命中");
    LRESULT(0)
}

/// C# SetTrayHostSizeData 的 Rust 版：把假 Shell_TrayWnd / TrayNotifyWnd 移动到 Dock
/// 的屏幕矩形 (left, top, width, height)。这是让 Shell_NotifyIconGetRect 与系统 flyout
/// 认 Dock 为新托盘宿主的关键——光回答 dwData=3 不够，宿主窗口本身必须在那个位置。
pub fn set_tray_host_size(left: i32, top: i32, width: i32, height: i32) {
    let prev = *HOST_RECT.lock().unwrap();
    *HOST_RECT.lock().unwrap() = (left, top, width, height);
    if prev != (left, top, width, height) {
        crate::vlog!("[trayhost] SetTrayHostSize ({left},{top},{width}x{height})");
    }
    unsafe {
        let own = *OWN_HWND.lock().unwrap();
        if own != 0 {
            let _ = SetWindowPos(
                HWND(own as *mut c_void), HWND::default(), left, top, width, height,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        let notify = *NOTIFY_HWND.lock().unwrap();
        if notify != 0 {
            let _ = SetWindowPos(
                HWND(notify as *mut c_void), HWND::default(), left, top, width, height,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
    }
}

/// Dock 每次重画后调用：把当前显示的图标屏幕矩形写进表，供位置查询回答。
pub fn set_placements(pl: Vec<Placement>) {
    if let Some(tab) = TABLE.lock().unwrap().clone() {
        tab.lock().unwrap().placements = pl;
    }
}

/// 取当前共享表句柄（Dock 用它订阅图标）。托盘托管未启动时返回 None。
pub fn table() -> Option<Arc<Mutex<State>>> {
    TABLE.lock().unwrap().clone()
}

/// 当前托管表里所有图标的进程 PID 去重列表（Dock 托盘实际显示的图标所属进程）。
/// 微信这类多进程应用用它过滤辅助子进程：托盘没图标的进程不用去枚举 UIA。
pub fn hosted_pids() -> Vec<u32> {
    let Some(tab) = TABLE.lock().unwrap().clone() else { return Vec::new() };
    let Ok(st) = tab.lock() else { return Vec::new() };
    let mut v: Vec<u32> = st.icons.iter().map(|i| i.pid).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// 给定 hwnd 是否是我们创建的假托盘窗口（隐藏真任务栏时必须排除它）。
pub fn is_own_hwnd(hwnd: HWND) -> bool {
    hwnd.0 as usize == *OWN_HWND.lock().unwrap()
}

impl Host {
    /// 拍一份当前图标表快照（主 crate 内不调用；`tools/trayhostcheck` 经 #[path] 使用）
    #[allow(dead_code)]
    pub fn snapshot(&self) -> Vec<Icon> {
        self.state.lock().unwrap().icons.clone()
    }
    /// 表的版本号（主 crate 内不调用；`tools/trayhostcheck` 经 #[path] 使用）
    #[allow(dead_code)]
    pub fn generation(&self) -> u64 {
        self.state.lock().unwrap().generation
    }
    /// 停止：恢复真任务栏（重播 TaskbarCreated，让应用把图标注册回去）
    pub fn stop(&self) {
        *QUITTING.lock().unwrap() = true;
        GUARD_QUIT.store(true, Ordering::Relaxed);
        // 关键：广播 TaskbarCreated 前先关闭消息转发，否则该注册消息被 SendMessage
        // 转给真任务栏 → explorer 重新注册 → 回发 → 无限递归栈溢出。
        FORWARD_ENABLED.store(false, Ordering::SeqCst);
        *OWN_HWND.lock().unwrap() = 0;
        *NOTIFY_HWND.lock().unwrap() = 0;
        unsafe {
            let tbc = RegisterWindowMessageW(w!("TaskbarCreated"));
            if tbc != 0 {
                let _ = SendNotifyMessageW(
                    HWND_BROADCAST, tbc, WPARAM(0), LPARAM(0),
                );
            }
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                HWND(self.hwnd as *mut c_void), WM_DESTROY, WPARAM(0), LPARAM(0),
            );
        }
        *TABLE.lock().unwrap() = None;
    }
}

