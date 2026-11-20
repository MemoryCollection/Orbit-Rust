//! 底部 Dock：钉选应用（.lnk）+ 运行中应用（按 exe 去重）+ 右侧托盘镜像。
//! 自绘图标与悬停底，点击钉选项启动、点击运行项切换窗口；托盘图标点击转发给
//! explorer 上真实的托盘图标（左键默认动作 / 右键在图标真实坐标发鼠标事件）。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::core::{w, Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{
    IShellLinkW, SHGetFileInfoW, SHGetImageList, ShellExecuteW, ShellLink, SHFILEINFOW, SHGFI_ICON,
    SHGFI_LARGEICON, SHGFI_SYSICONINDEX, SHIL_JUMBO, SLGP_RAWPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, EnumWindows, GetClassNameW, GetForegroundWindow,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, HMENU, IsIconic, IsWindowVisible, PostMessageW, RegisterWindowMessageW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, GWLP_USERDATA,
    GW_OWNER, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, SWP_NOACTIVATE, SW_HIDE, SW_RESTORE,
    SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WM_CREATE, WM_DESTROY, WM_DISPLAYCHANGE, WM_LBUTTONDOWN,
    WM_MOUSEMOVE, WM_NCHITTEST, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::config as cfg;
use crate::render;
use crate::tray::TrayMirror;
use crate::flyout;
use crate::trayhost;
use crate::traymsg;

/// Dock 窗口的定时器 ID
const T_RESCAN: usize = 3;
/// 托盘闪烁采样（应用用一闪一闪提示新消息，跟随真实状态就得采得够快）
const T_FLASH: usize = 5;
/// 后台扫完"图标回调消息"后叫醒窗口线程（lParam 不用，下标在 wParam）
const WM_TRAY_SCAN: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 0x71;

/// 点击目标
enum Target {
    /// 钉选快捷方式路径
    Lnk(PathBuf),
    /// 运行中窗口句柄
    Window(HWND),
}

struct DockItem {
    name: String,
    /// 悬停提示文字：运行中用窗口标题，钉选项用快捷方式名
    tip: String,
    /// 归一化名字（托盘名字兜底匹配用）
    norm: String,
    /// 该项代表的程序（exe 全路径小写；钉选项取快捷方式目标）
    exe: String,
    /// 是否代表一个可见窗口：托盘里的后台进程没有窗口，不能算进去
    running: bool,
    target: Target,
    rgba: Vec<u8>,
}

pub struct Dock {
    hwnd: HWND,
    /// 悬停提示条窗口（独立的点击穿透分层窗，画在 Dock 上方）
    tip: HWND,
    scale: f32,
    items: Vec<DockItem>,
    hovered: Option<usize>,
    mouse_tracked: bool,
    /// 每个应用图标槽位 [x, y, size]（client 坐标）
    rects: Vec<[f32; 3]>,
    /// 每个托盘图标槽位 [x, y, size]（client 坐标）
    tray_rects: Vec<[f32; 3]>,
    /// 托盘镜像（UIA 枚举 + PrintWindow 抠图）
    tray: TrayMirror,
    /// 隐藏掉"和 Dock 图标重复"的之后，实际显示的托盘下标（按显示顺序）
    tray_shown: Vec<usize>,
    hovered_tray: Option<usize>,
    size: (i32, i32),
    /// 上次扫描的 (名称, 目标) 签名，变化才重绘
    last_sig: Vec<(String, String)>,
    /// explorer 重启后托盘会重建，靠它清缓存
    taskbar_created: u32,
    /// 右键按下的托盘下标：等物理键抬起后再注入（按下时注入会被当成同一次按键吞掉）
    pending_right: Option<usize>,
    /// 图标中心坐标 → 回调目标（宿主窗口 + 消息号 + uid + 协议版本）。
    /// 用坐标当键而不是下标：托盘重建后下标会变，坐标不会错位
    tray_targets: HashMap<(i32, i32), traymsg::Target>,
    /// 正在后台扫内存找回调目标的图标
    tray_scanning: HashSet<(i32, i32)>,
    /// 后台扫描结果：线程写、窗口线程取
    tray_scan_result: Arc<Mutex<HashMap<(i32, i32), Option<traymsg::Raw>>>>,
    /// 托管模式下与 tray.items 一一对应的回调目标（render 后写进 tray_targets）
    host_targets: Vec<Option<traymsg::Target>>,
    /// 已经对哪个图标发过"悬停进入"（离开时要补 popupclose）
    hover_sent: Option<(i32, i32)>,
    /// 悬停期间向同一图标转发 WM_MOUSEMOVE 的节流时刻（真托盘大约 150ms 一次）
    last_hover_move: Option<Instant>,
    /// 上次左键按下的时刻（判双击）
    last_lclick: Option<Instant>,
}

pub unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CREATE {
            let cs = *(l.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return LRESULT(0);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dock;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let dock = &mut *ptr;
        match msg {
            WM_TIMER if w.0 == T_RESCAN => {
                let apps = dock.scan();
                let tray = dock.poll_tray();
                if apps || tray {
                    dock.render();
                }
                LRESULT(0)
            }
            WM_TIMER if w.0 == T_FLASH => {
                if dock.poll_flash() {
                    dock.render();
                }
                LRESULT(0)
            }
            WM_NCHITTEST => {
                let mut pt = POINT {
                    x: (l.0 & 0xFFFF) as u16 as i16 as i32,
                    y: ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
                };
                let mut wr = RECT::default();
                let _ = GetWindowRect(hwnd, &mut wr);
                pt.x -= wr.left;
                pt.y -= wr.top;
                let (dw, dh) = dock.size;
                if inside_round(pt.x as f32, pt.y as f32, dw as f32, dh as f32, cfg::DOCK_RADIUS * dock.scale) {
                    LRESULT(HTCLIENT as isize)
                } else {
                    LRESULT(HTTRANSPARENT as isize)
                }
            }
            WM_MOUSEMOVE => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                let idx = dock.index_at(x as f32, y as f32);
                let tidx = dock.tray_index_at(x as f32, y as f32);
                if idx != dock.hovered || tidx != dock.hovered_tray {
                    dock.hovered = idx;
                    dock.hovered_tray = tidx;
                    dock.render();
                    // 悬停即显示提示条（托盘名字来自 UIA，任务图标用程序名）
                    dock.update_tip();
                    dock.forward_hover();
                }
                // 指针还停在同一个托盘图标上：像真托盘那样隔一会儿补一条 WM_MOUSEMOVE，
                // 应用（微信）靠它维持 hover 跟踪，只发一次 enter 不够
                if tidx.is_some() {
                    let now = Instant::now();
                    let due = dock
                        .last_hover_move
                        .map_or(true, |t| now.duration_since(t).as_millis() >= 150);
                    if due {
                        if let Some(key) = dock.hover_center() {
                            if let Some(t) = dock.tray_targets.get(&key).copied() {
                                dock.last_hover_move = Some(now);
                                traymsg::hover_move(&t, key);
                            }
                        }
                    }
                }
                if !dock.mouse_tracked {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    dock.mouse_tracked = true;
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                dock.mouse_tracked = false;
                dock.hide_tip();
                // 注意不能写成 `a.take().is_some() || b.take().is_some()`：
                // 短路后第二个 take 不会执行，托盘悬停状态会卡住
                let had_app = dock.hovered.take().is_some();
                let had_tray = dock.hovered_tray.take().is_some();
                // 先清悬停态再转发：否则 forward_hover 会以为还停着，不发 leave
                dock.forward_hover();
                if had_app || had_tray {
                    dock.render();
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                if let Some(idx) = dock.tray_index_at(x as f32, y as f32) {
                    // 有回调目标就直接投消息（不移动光标）；没有才退回 UIA 默认动作
                    let now = Instant::now();
                    let dbl = dock
                        .last_lclick
                        .map_or(false, |t| {
                            now.duration_since(t).as_millis() as u32
                                <= windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime()
                        });
                    dock.last_lclick = Some(now);
                    let c = dock.tray_center(idx);
                    if let Some(t) = dock.tray_targets.get(&c).copied() {
                        crate::vlog!("[dock] 左键托盘#{idx}（回调消息{}）", if dbl { " 双击" } else { "" });
                        traymsg::click(&t, c, dbl);
                    } else if let Some(&real) = dock.tray_shown.get(idx) {
                        crate::vlog!("[dock] 左键托盘 显示#{idx} → 实际#{real}（UIA）");
                        dock.tray.click(real, false);
                    }
                } else if let Some(idx) = dock.index_at(x as f32, y as f32) {
                    dock.activate(idx);
                }
                dock.hide_tip();
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                // 只记下来，等 WM_RBUTTONUP 再注入：按下瞬间注入的右键会和这次物理按键
                // 合成一次"按下-抬起"，真实托盘图标收不到点击
                dock.pending_right = dock.tray_index_at(x as f32, y as f32);
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                if let Some(idx) = dock.pending_right.take() {
                    dock.hide_tip();
                    let c = dock.tray_center(idx);
                    match dock.tray_targets.get(&c).copied() {
                        Some(t) => {
                            crate::vlog!("[dock] 右键托盘#{idx}（回调消息）");
                            traymsg::right_click(&t, c);
                        }
                        None => {
                            // 还不知道回调消息号：先退回"移过去点一下"，同时把扫描排上
                            if let Some(&real) = dock.tray_shown.get(idx) {
                                crate::vlog!("[dock] 右键托盘#{idx} → 实际#{real}（UIA）");
                                dock.tray.click(real, true);
                            }
                            dock.spawn_target_scan(idx, c);
                        }
                    }
                }
                LRESULT(0)
            }
            WM_TRAY_SCAN => {
                let key = (w.0 as i32, l.0 as i32);
                let got = dock.tray_scan_result.lock().ok().and_then(|mut m| m.remove(&key));
                dock.tray_scanning.remove(&key);
                match got {
                    Some(Some(raw)) => {
                        let t = traymsg::Target::from_raw(raw);
                        crate::tlog!("[dock] 图标{:?} 回调目标 msg={:#x} uid={} v{}", key, t.msg, t.uid, t.ver);
                        dock.tray_targets.insert(key, t);
                        // 扫描是异步的：结果回来时用户可能还停在这个图标上
                        if dock.hover_center() == Some(key) {
                            let c = traymsg::icon_center_of(t).unwrap_or(key);
                            traymsg::hover(&t, c);
                            dock.hover_sent = Some(key);
                            if let Some(hidx) = dock.hovered_tray {
                                dock.watch_flyout(t, hidx);
                            }
                        }
                    }
                    _ => crate::tlog!("[dock] 图标{:?} 找不到回调目标，退回 UIA 点击", key),
                }
                LRESULT(0)
            }
            WM_DISPLAYCHANGE => {
                dock.render();
                LRESULT(0)
            }
            m if m == dock.taskbar_created && m != 0 => {
                // explorer 重启：托盘 UIA 元素全换新，签名作废
                dock.tray = TrayMirror::default();
                if dock.poll_tray() {
                    dock.render();
                }
                LRESULT(0)
            }
            WM_DESTROY => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}

impl Dock {
    pub fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            tip: HWND::default(),
            scale: 1.0,
            items: Vec::new(),
            hovered: None,
            mouse_tracked: false,
            rects: Vec::new(),
            tray_rects: Vec::new(),
            tray: TrayMirror::default(),
            tray_shown: Vec::new(),
            hovered_tray: None,
            size: (0, 0),
            last_sig: Vec::new(),
            taskbar_created: 0,
            pending_right: None,
            tray_targets: HashMap::new(),
            tray_scanning: HashSet::new(),
            tray_scan_result: Arc::new(Mutex::new(HashMap::new())),
            host_targets: Vec::new(),
            hover_sent: None,
            last_hover_move: None,
            last_lclick: None,
        }
    }

    pub fn init(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        self.scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
        self.taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        self.tip = unsafe { create_tip_window() };
        self.tray.attach_host();
        self.scan();
        self.poll_tray();
        self.render();
    }

    /// 轮询托盘镜像；返回是否有变化（需要重绘）
    fn poll_tray(&mut self) -> bool {
        if !cfg::SHOW_TRAY {
            return false;
        }
        let px = (cfg::DOCK_TRAY_ICON * self.scale).round() as i32;
        let changed = self.tray.poll(px);
        if changed {
            // 托管模式：回调消息/GUID/版本都来自 NOTIFYICONDATA，直接建目标，
            // 不用再挖内存。坐标键用图标"应该"在的位置，render 后会用真实槽位修正。
            if self.tray.hosted() {
                self.build_host_targets();
            }
        }
        changed
    }

    /// 快速采样托盘图标的"亮/灭"（应用用闪烁提示新消息）；返回是否需要重绘
    fn poll_flash(&mut self) -> bool {
        if !cfg::SHOW_TRAY {
            return false;
        }
        let px = (cfg::DOCK_TRAY_ICON * self.scale).round() as i32;
        if self.tray.hosted() {
            // 托管模式的 dim 来自 NIM_MODIFY(icon=0)。微信每帧只有 200~300ms，
            // 必须走这个高频定时器同步，不能等 2s 的整表重扫（会漏帧、卡在灭态）。
            let changed = self.tray.poll(px);
            if changed {
                self.build_host_targets();
            }
            return changed;
        }
        self.tray.sample()
    }

    /// 从托管图标表直接建回调目标（与 tray.items 一一对应）。
    /// 真正的坐标键在 render() 里按 Dock 槽位写入 tray_targets。
    fn build_host_targets(&mut self) {
        let mut v = Vec::with_capacity(self.tray.items.len());
        for it in &self.tray.items {
            let t = it.host.as_ref().map(|h| {
                let ver = if h.ver == 3 { 3 } else { 4 };
                traymsg::Target {
                    pid: h.pid,
                    hwnd: windows::Win32::Foundation::HWND(h.hwnd as *mut core::ffi::c_void),
                    msg: h.msg,
                    uid: h.uid,
                    ver,
                }
            });
            v.push(t);
        }
        self.host_targets = v;
    }

    /// 悬停态变化时把"指针在图标上/离开"转发给真实托盘图标。
    /// 走的是图标注册时的回调消息（wParam/lParam 按协议版本编码），**不动光标**，
    /// 也不用管任务栏是否可见——这也正是"悬停即弹卡片"能成立的前提。
    fn forward_hover(&mut self) {
        if !cfg::TRAY_FORWARD_HOVER {
            return;
        }
        let want = self.hover_center();
        if self.hover_sent == want {
            return;
        }
        if let Some(prev) = self.hover_sent.take() {
            if let Some(t) = self.tray_targets.get(&prev).copied() {
                crate::vlog!("[dock] 悬停离开 {:?}", prev);
                traymsg::leave(&t, prev);
            }
            self.last_hover_move = None;
        }
        let Some(key) = want else { return };
        let Some(i) = self.hovered_tray else { return };
        crate::tlog!("[dock] 悬停转发开始 key={key:?} idx={i} 已知目标={}", self.tray_targets.contains_key(&key));
        match self.tray_targets.get(&key).copied() {
            Some(t) => {
                // 托管模式：坐标就用我们上报给宿主的 Placement 中心（key），
                // 应用查 Shell_NotifyIconGetRect 得到的也是它，前后一致才认悬停。
                // 旧 UIA 模式才用图标在真任务栏里的矩形中心。
                let c = if self.tray.hosted() { key } else { traymsg::icon_center_of(t).unwrap_or(key) };
                crate::vlog!("[dock] 悬停转发 {c:?} → hwnd={:#x} msg={:#x} uid={} v{}", t.hwnd.0 as usize, t.msg, t.uid, t.ver);
                traymsg::hover(&t, c);
                self.hover_sent = Some(key);
                self.watch_flyout(t, i);
            }
            // 托管模式下所有目标在 build_host_targets 里已建好；走到这里说明图标刚变动，
            // 下一轮 poll_tray 会补上。旧模式才去挖内存。
            None => if !self.tray.hosted() { self.spawn_target_scan(i, key) },
        }
    }

    /// 应用在悬停时会自己弹一个小预览窗，位置在旧托盘角；把它摆到 Dock 上方。
    /// 真正的活儿在后台线程里（`flyout::watch`），这里只把 Dock 的屏幕矩形递过去。
    fn watch_flyout(&self, t: traymsg::Target, idx: usize) {
        if !cfg::TRAY_RELOCATE_FLYOUT {
            return;
        }
        let label = self
            .tray_shown
            .get(idx)
            .and_then(|&real| self.tray.items.get(real))
            .map(|it| it.name.clone())
            .unwrap_or_default();
        let mut cap = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut cap) }.is_err() {
            return;
        }
        flyout::watch(t.pid, t.hwnd, cap, &label);
    }

    /// 当前悬停的托盘图标中心（屏幕坐标）
    fn hover_center(&self) -> Option<(i32, i32)> {
        self.hovered_tray.map(|i| self.tray_center(i))
    }

    /// 图标中心（屏幕坐标）。坐标用托盘图标自己的矩形，不用真实光标位置
    fn tray_center(&self, idx: usize) -> (i32, i32) {
        if let Some(&real) = self.tray_shown.get(idx) {
            if let Some(it) = self.tray.items.get(real) {
                let r = it.icon_rect;
                return ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
            }
        }
        let r = self.tray_rects.get(idx).copied().unwrap_or([0.0, 0.0, 0.0]);
        let mut wr = RECT::default();
        let _ = unsafe { GetWindowRect(self.hwnd, &mut wr) };
        (wr.left + (r[0] + r[2] / 2.0) as i32, wr.top + (r[1] + r[2] / 2.0) as i32)
    }

    /// 后台线程去目标进程里找这个图标的 (宿主窗口, 回调消息号, uid, 协议版本)
    fn spawn_target_scan(&mut self, idx: usize, key: (i32, i32)) {
        if !self.tray_scanning.insert(key) {
            return;
        }
        let Some(&real) = self.tray_shown.get(idx) else {
            self.tray_scanning.remove(&key);
            return;
        };
        let Some(item) = self.tray.items.get(real) else {
            self.tray_scanning.remove(&key);
            return;
        };
        let name = item.name.clone();
        let icon = item.icon_rect;
        let exe = self.tray.exe_of(&name).unwrap_or_default();
        if exe.is_empty() {
            crate::tlog!("[dock] 图标{key:?}（{name}）认不出程序，不做悬停转发");
            self.tray_scanning.remove(&key);
            return;
        }
        crate::tlog!("[dock] 后台扫描 {name}（{exe}）的图标回调消息");
        let shared = self.tray_scan_result.clone();
        let raw = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let t = traymsg::target(&exe, icon, 0).map(|t| t.raw());
            if let Ok(mut m) = shared.lock() {
                m.insert(key, t);
            }
            let hwnd = HWND(raw as *mut core::ffi::c_void);
            unsafe {
                let _ = PostMessageW(hwnd, WM_TRAY_SCAN, WPARAM(key.0 as usize), LPARAM(key.1 as isize));
            }
        });
    }

    /// 重扫钉选 + 运行中应用；签名有变化才返回 true。
    pub fn scan(&mut self) -> bool {
        let px = (cfg::DOCK_ICON * self.scale).round() as i32;
        let mut items: Vec<DockItem> = Vec::new();
        // 有可见窗口的应用（exe 小写 → 一个窗口）；托盘去重要靠它判断"程序是否已在 Dock 上"
        let run_map = running_apps();

        // 钉选应用（顺序按文件名）
        let mut pinned_exes: Vec<String> = Vec::new();
        // 解析不出目标的钉选项（CLSID 快捷方式，如 File Explorer）靠名字匹配运行项
        let mut pinned_names: Vec<String> = Vec::new();
        for lnk in pinned_links() {
            let target = shell_link_target(&lnk).map(|t| expand_env(&t)).unwrap_or_default();
            let name = lnk.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let rgba = pinned_icon(&lnk, &target, px);
            if target.is_empty() {
                let n = normalize_name(&name);
                if n.len() >= 8 {
                    pinned_names.push(n);
                }
            }
            let exe = target.to_lowercase();
            let norm = normalize_name(&name);
            pinned_exes.push(exe.clone());
            items.push(DockItem {
                tip: name.clone(),
                name,
                norm,
                running: !exe.is_empty() && run_map.contains_key(&exe),
                exe,
                target: Target::Lnk(lnk),
                rgba,
            });
        }

        // 运行中应用（exe 去重，已钉选的跳过）
        for (exe, hwnd) in &run_map {
            if pinned_exes.iter().any(|p| !p.is_empty() && p == exe) {
                continue;
            }
            let name = PathBuf::from(exe)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let norm = normalize_name(&name);
            if pinned_names.iter().any(|p| norm.contains(p.as_str()) || p.contains(&norm)) {
                continue;
            }
            // 大图标从 exe 取；拿不到再退回窗口图标
            let mut rgba = large_icon_of_path(std::path::Path::new(exe), px);
            if rgba.is_empty() {
                rgba = unsafe { render::window_icon(*hwnd) }
                    .and_then(|h| render::capture_icon_rgba(h, px))
                    .unwrap_or_default();
            }
            items.push(DockItem {
                tip: window_title(*hwnd).unwrap_or_else(|| name.clone()),
                name,
                norm,
                exe: exe.clone(),
                running: true,
                target: Target::Window(*hwnd),
                rgba,
            });
        }

        let sig: Vec<(String, String)> = items
            .iter()
            .map(|it| {
                let key = match &it.target {
                    Target::Lnk(p) => format!("lnk:{}", p.display()),
                    Target::Window(h) => format!("win:{:?}", h.0 as usize),
                };
                (it.name.clone(), key)
            })
            .collect();

        let changed = sig != self.last_sig;
        self.last_sig = sig;
        self.items = items;
        if self.hovered.map(|i| i >= self.items.len()).unwrap_or(false) {
            self.hovered = None;
        }
        changed
    }

    pub fn render(&mut self) {
        // 先定"哪些托盘图标要显示"：和 Dock 图标重复的藏掉
        self.refresh_tray_shown();
        let n = self.items.len();
        let n_tray = if cfg::SHOW_TRAY { self.tray_shown.len() } else { 0 };
        let w = render::dock_width(n, n_tray, self.scale).ceil() as i32;
        let h = (cfg::DOCK_H * self.scale).ceil() as i32;
        self.size = (w, h);

        if self.hovered.map(|i| i >= n).unwrap_or(false) {
            self.hovered = None;
        }
        if self.hovered_tray.map(|i| i >= n_tray).unwrap_or(false) {
            self.hovered_tray = None;
        }

        let icons: Vec<&[u8]> = self.items.iter().map(|it| it.rgba.as_slice()).collect();
        // 微信有新消息时托盘在"正常图标/空白"间交替提示，胶囊按同一频率同步：
        // dim 帧画全透明像素（图标消失），亮帧画真实图标。
        let shown_items: Vec<&crate::tray::TrayItem> = self
            .tray_shown
            .iter()
            .take(n_tray)
            .filter_map(|&i| self.tray.items.get(i))
            .collect();
        let blank = vec![0u8; shown_items.first().map(|it| it.rgba.len()).unwrap_or(0)];
        let tray_icons: Vec<&[u8]> = shown_items
            .iter()
            .map(|it| if it.dim { blank.as_slice() } else { it.rgba.as_slice() })
            .collect();
        let out = render::draw_dock(
            w, h, self.scale, &icons, self.hovered, &tray_icons, self.hovered_tray,
        );
        self.rects = out.rects.clone();
        self.tray_rects = out.tray_rects.clone();
        let bgra = render::to_premultiplied_bgra(&out.rgba);
        render::present(self.hwnd, &bgra, w, h);
        crate::vlog!(
            "[dock] w={w} apps={} tray={}/{} rects={:?} tray_rects={:?} hovered_tray={:?}",
            self.items.len(), n_tray, self.tray.items.len(), self.rects, self.tray_rects, self.hovered_tray
        );

        // 定位：水平按"屏幕矩形"居中（与顶部胶囊同一基准，任务栏在侧边时也不会偏），
        // 垂直贴屏幕底（原生任务栏还占着工作区时，落在它上方）
        let mi = crate::window::primary_monitor();
        let bottom = if crate::hide::is_active() { mi.rcMonitor.bottom } else { mi.rcWork.bottom };
        let mon = mi.rcMonitor;
        let x = mon.left + (mon.right - mon.left - w) / 2;
        let y = bottom - h - (cfg::DOCK_BOTTOM_MARGIN as f32 * self.scale) as i32;
        unsafe {
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
        }

        // 托管模式：把假 Shell_TrayWnd 移动到 Dock 矩形（对齐 C# SetTrayHostSizeData，
        // 让 Shell_NotifyIconGetRect 认 Dock 为新托盘宿主），把每个图标屏幕矩形告诉宿主，
        // 并按真实槽位中心重建 tray_targets（悬停/点击投消息用这个坐标）。
        if self.tray.hosted() {
            crate::trayhost::set_tray_host_size(x, y, w, h);
            self.sync_host_layout(x, y);
        }
    }

    /// 托管模式下按 Dock 真实槽位更新目标坐标键 + 回报图标 Placement。
    fn sync_host_layout(&mut self, wx: i32, wy: i32) {
        let mut targets = HashMap::new();
        let mut placements = Vec::new();
        for (shown, &real) in self.tray_shown.iter().enumerate() {
            let Some(r) = self.tray_rects.get(shown).copied() else { continue };
            let (cx, cy) = (wx + (r[0] + r[2] / 2.0) as i32, wy + (r[1] + r[2] / 2.0) as i32);
            let center = (cx, cy);
            let rect = (cx - (r[2] / 2.0) as i32, cy - (r[2] / 2.0) as i32,
                        cx + (r[2] / 2.0) as i32, cy + (r[2] / 2.0) as i32);
            // 写回 TrayItem.icon_rect：tray_center()/icon 位置都读它
            if let Some(it) = self.tray.items.get_mut(real) {
                it.icon_rect = RECT { left: rect.0, top: rect.1, right: rect.2, bottom: rect.3 };
            }
            if let Some(Some(t)) = self.host_targets.get(real) {
                targets.insert(center, *t);
            }
            if let Some(ic) = self.tray.items.get(real).and_then(|it| it.host.as_ref()) {
                placements.push(trayhost::Placement {
                    hwnd: ic.hwnd, uid: ic.uid, guid: ic.guid,
                    left: rect.0, top: rect.1, right: rect.2, bottom: rect.3,
                });
            }
        }
        self.tray_targets = targets;
        crate::trayhost::set_placements(placements);
    }

    /// 托盘图标去重：Dock 上已有的程序不再重复显示托盘图标。
    /// 多开（同一个 exe 的托盘图标数和 Dock 图标数对不上）时全部保留——
    /// 分不清该藏哪一个，藏错了会让某个实例彻底没有入口（微信多开就是这样）。
    fn refresh_tray_shown(&mut self) {
        let n = self.tray.items.len();
        if !cfg::SHOW_TRAY {
            self.tray_shown.clear();
            return;
        }
        // Dock 里"代表可见窗口"的程序 → 图标数
        let mut on_dock: BTreeMap<String, usize> = BTreeMap::new();
        for it in &self.items {
            if it.running && !it.exe.is_empty() {
                *on_dock.entry(it.exe.clone()).or_insert(0) += 1;
            }
        }
        if n == 0 || on_dock.is_empty() {
            self.tray_shown = (0..n).collect();
            return;
        }

        // 每个托盘图标归属到哪个程序：进程 exe 优先；explorer 托管的（XAML 树在导出器
        // 进程里）拿名字到 Dock 项里找
        let keys: Vec<Option<String>> = self
            .tray
            .items
            .iter()
            .map(|t| {
                if !t.exe.is_empty() && !t.exe.ends_with("explorer.exe") {
                    return Some(t.exe.clone());
                }
                let norm = normalize_name(&t.name);
                if norm.len() < 3 {
                    return None;
                }
                self.items
                    .iter()
                    .filter(|d| d.running && !d.exe.is_empty() && !d.norm.is_empty())
                    .find(|d| d.norm.contains(&norm) || norm.contains(&d.norm))
                    .map(|d| d.exe.clone())
            })
            .collect();

        let mut tray_count: BTreeMap<String, usize> = BTreeMap::new();
        for k in keys.iter().flatten() {
            *tray_count.entry(k.clone()).or_insert(0) += 1;
        }

        let mut shown = Vec::with_capacity(n);
        for (i, key) in keys.iter().enumerate() {
            let dup = key.as_ref().map_or(false, |k| {
                let dock = on_dock.get(k).copied().unwrap_or(0);
                let tray = tray_count.get(k).copied().unwrap_or(0);
                dock > 0 && dock == tray
            });
            if !dup {
                shown.push(i);
            }
        }
        if shown.len() != n {
            crate::vlog!(
                "[dock] 托盘去重：{}/{} 个保留（Dock 同程序：{:?}）",
                shown.len(), n, on_dock
            );
        }
        self.tray_shown = shown;
    }

    /// 根据当前悬停项显示提示条（托盘用 tooltip 名字，任务图标用程序名）
    fn update_tip(&mut self) {
        let (text, rect) = if let Some(i) = self.hovered_tray {
            let rect = self.tray_rects.get(i).copied();
            let item = self
                .tray_shown
                .get(i)
                .and_then(|&k| self.tray.items.get(k))
                .map(|it| it.name.clone())
                .unwrap_or_default();
            match rect {
                Some(r) => (item, r),
                None => return self.hide_tip(),
            }
        } else if let Some(i) = self.hovered {
            match (self.rects.get(i).copied(), self.items.get(i)) {
                (Some(r), Some(it)) => (it.tip.clone(), r),
                _ => return self.hide_tip(),
            }
        } else {
            return self.hide_tip();
        };
        if text.trim().is_empty() {
            return self.hide_tip();
        }
        let mut wr = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut wr);
        }
        let cx = wr.left + (rect[0] + rect[2] / 2.0) as i32;
        let top = wr.top + rect[1] as i32;
        self.show_tip(&text, cx, top);
    }

    fn show_tip(&mut self, text: &str, cx: i32, top: i32) {
        if self.tip.is_invalid() {
            crate::vlog!("[dock] 提示条窗口无效，跳过");
            return;
        }
        let (w, h, rgba) = render::draw_tooltip(text, self.scale);
        if w <= 0 || h <= 0 {
            crate::vlog!("[dock] 提示条绘制失败：{text}");
            return;
        }
        let bgra = render::to_premultiplied_bgra(&rgba);
        render::present(self.tip, &bgra, w, h);
        let mon = crate::window::primary_monitor().rcMonitor;
        let x = (cx - w / 2)
            .clamp(mon.left + 4, (mon.right - w - 4).max(mon.left + 4));
        let y = top - h - (cfg::TIP_GAP as f32 * self.scale) as i32;
        unsafe {
            let _ = SetWindowPos(self.tip, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
            let _ = ShowWindow(self.tip, SW_SHOWNOACTIVATE);
        }
        crate::vlog!("[dock] 提示条 \"{text}\" {w}x{h} @({x},{y}) tip={:?}", self.tip.0);
    }

    fn hide_tip(&self) {
        if !self.tip.is_invalid() {
            unsafe {
                let _ = ShowWindow(self.tip, SW_HIDE);
            }
        }
    }

    fn index_at(&self, x: f32, y: f32) -> Option<usize> {
        self.rects.iter().position(|r| {
            let s = r[2];
            x >= r[0] && x <= r[0] + s && y >= r[1] && y <= r[1] + s
        })
    }

    fn tray_index_at(&self, x: f32, y: f32) -> Option<usize> {
        self.tray_rects.iter().position(|r| {
            let s = r[2];
            x >= r[0] && x <= r[0] + s && y >= r[1] && y <= r[1] + s
        })
    }

    fn activate(&self, idx: usize) {
        let Some(item) = self.items.get(idx) else { return };
        match &item.target {
            Target::Lnk(path) => {
                let wide = to_wide(path.as_os_str());
                unsafe {
                    ShellExecuteW(
                        HWND::default(),
                        w!("open"),
                        PCWSTR(wide.as_ptr()),
                        PCWSTR::null(),
                        PCWSTR::null(),
                        SW_SHOWNORMAL,
                    );
                }
            }
            Target::Window(hwnd) => unsafe {
                if IsIconic(*hwnd).as_bool() {
                    let _ = ShowWindow(*hwnd, SW_RESTORE);
                }
                // 绕过前台锁：临时把本线程输入队列挂到当前前台线程
                let fg = GetForegroundWindow();
                let tid_fg = GetWindowThreadProcessId(fg, None);
                let tid_me = GetCurrentThreadId();
                let _ = AttachThreadInput(tid_me, tid_fg, true);
                let _ = SetForegroundWindow(*hwnd);
                let _ = AttachThreadInput(tid_me, tid_fg, false);
            },
        }
    }
}

/// 提示条窗口：分层 + 置顶 + 点击穿透，不抢焦点也不会挡鼠标
unsafe fn create_tip_window() -> HWND {
    let hinstance = GetModuleHandleW(None).unwrap_or_default();
    CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
        w!("ORBIT_TIP"),
        PCWSTR::null(),
        WS_POPUP,
        0,
        0,
        10,
        10,
        HWND::default(),
        HMENU::default(),
        hinstance,
        None,
    )
    .unwrap_or_default()
}

// ---------- 数据采集 ----------

/// `%APPDATA%\...\User Pinned\TaskBar` 下的 .lnk（Explorer 的钉选来源）
fn pinned_links() -> Vec<PathBuf> {
    let Ok(appdata) = std::env::var("APPDATA") else { return Vec::new() };
    let dir = PathBuf::from(appdata)
        .join("Microsoft")
        .join("Internet Explorer")
        .join("Quick Launch")
        .join("User Pinned")
        .join("TaskBar");
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("lnk")).unwrap_or(false))
        .collect();
    out.sort();
    out
}

/// 运行中的应用：exe 小写路径 → 该应用的一个可见窗口。已过滤工具窗/幽灵窗/无标题窗。
fn running_apps() -> BTreeMap<String, HWND> {
    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut hwnds as *mut Vec<HWND> as isize));
    }
    let mut map = BTreeMap::new();
    for hwnd in hwnds {
        if let Some(exe) = exe_path(hwnd) {
            map.entry(exe.to_lowercase()).or_insert(hwnd);
        }
    }
    map
}

/// 窗口标题（悬停提示用；空标题返回 None）
fn window_title(hwnd: HWND) -> Option<String> {
    unsafe {
        let n = GetWindowTextLengthW(hwnd);
        if n <= 0 {
            return None;
        }
        let mut buf = vec![0u16; n as usize + 1];
        let len = GetWindowTextW(hwnd, &mut buf);
        let s = String::from_utf16_lossy(&buf[..len.max(0) as usize]).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }
}

unsafe extern "system" fn enum_proc(hwnd: HWND, l: LPARAM) -> BOOL {
    unsafe {
        let out = &mut *(l.0 as *mut Vec<HWND>);
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }
        if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return true.into();
        }
        // 有 owner 的是对话框/弹出窗
        if let Ok(owner) = windows::Win32::UI::WindowsAndMessaging::GetWindow(hwnd, GW_OWNER) {
            if !owner.is_invalid() {
                return true.into();
            }
        }
        // UWP 关闭后残留的 cloaked 幽灵窗口
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return true.into();
        }
        let mut buf = [0u16; 128];
        let n = GetClassNameW(hwnd, &mut buf);
        let cls = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        if matches!(
            cls.as_str(),
            "Progman"
                | "WorkerW"
                | "Shell_TrayWnd"
                | "Shell_SecondaryTrayWnd"
                | "NotifyIconOverflowWindow"
                | "Windows.UI.Core.CoreWindow"
                | "ORBIT_CAPSULE"
                | "ORBIT_DOCK"
        ) {
            return true.into();
        }
        if GetWindowTextLengthW(hwnd) == 0 {
            return true.into();
        }
        out.push(hwnd);
        true.into()
    }
}

fn exe_path(hwnd: HWND) -> Option<String> {    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(proc, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(proc);
        if ok && len > 0 {
            Some(String::from_utf16_lossy(&buf[..len as usize]))
        } else {
            None
        }
    }
}

/// 解析 .lnk 的目标路径（UWP 快捷方式可能为空）
fn shell_link_target(lnk: &std::path::Path) -> Option<String> {
    let wide = to_wide(lnk.as_os_str());
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(PCWSTR(wide.as_ptr()), STGM_READ).ok()?;
        let mut buf = [0u16; 1024];
        let mut fd = windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW::default();
        link.GetPath(&mut buf, &mut fd, SLGP_RAWPATH.0 as u32).ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..len]);
        if s.is_empty() { None } else { Some(s) }
    }
}

/// 钉选图标：优先用快捷方式目标（避免快捷方式小箭头），否则退回 .lnk 自身图标
fn pinned_icon(lnk: &std::path::Path, target: &str, px: i32) -> Vec<u8> {
    if !target.is_empty() {
        let v = large_icon_of_path(std::path::Path::new(target), px);
        if !v.is_empty() {
            return v;
        }
    }
    large_icon_of_path(lnk, px)
}

/// 取 256px 系统图像列表图标并高质量缩放到 px；取不到则退回 32px 直接绘制
pub(crate) fn large_icon_of_path(path: &std::path::Path, px: i32) -> Vec<u8> {
    let wide = to_wide(path.as_os_str());
    unsafe {
        let mut info = SHFILEINFOW::default();
        let ok = SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            Default::default(),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX | SHGFI_LARGEICON,
        );
        if ok != 0 {
            if let Ok(list) = SHGetImageList::<IImageList>(SHIL_JUMBO as i32) {
                let (mut cx, mut cy) = (0i32, 0i32);
                if list.GetIconSize(&mut cx, &mut cy).is_ok() && cx > 0 && cy > 0 {
                    if let Ok(hicon) = list.GetIcon(info.iIcon, ILD_TRANSPARENT.0) {
                        let big = render::capture_icon_rgba(hicon, cx);
                        let _ = DestroyIcon(hicon);
                        if let Some(big) = big {
                            return render::resize_rgba(&big, cx, cy, px, px);
                        }
                    }
                }
            }
        }

        let mut fallback = SHFILEINFOW::default();
        let ok = SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            Default::default(),
            Some(&mut fallback),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if ok == 0 || fallback.hIcon.is_invalid() {
            return Vec::new();
        }
        let rgba = render::capture_icon_rgba(fallback.hIcon, px).unwrap_or_default();
        let _ = DestroyIcon(fallback.hIcon);
        rgba
    }
}

/// 展开 `%windir%` 这类环境变量（.lnk 的目标常带变量）
fn expand_env(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let wide = to_wide(std::ffi::OsStr::new(s));
    let mut buf = [0u16; 1024];
    unsafe {
        let n = ExpandEnvironmentStringsW(PCWSTR(wide.as_ptr()), Some(&mut buf));
        if n > 0 && (n as usize) <= buf.len() {
            String::from_utf16_lossy(&buf[..n as usize - 1])
        } else {
            s.to_string()
        }
    }
}

fn to_wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// 归一化应用名（只留字母数字、小写），用于 CLSID 钉选项与运行项的名字匹配
pub(crate) fn normalize_name(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase()
}

/// 圆角矩形命中（四角圆弧外穿透）
fn inside_round(x: f32, y: f32, w: f32, h: f32, r: f32) -> bool {
    if x < 0.0 || x > w || y < 0.0 || y > h {
        return false;
    }
    let r = r.min(w / 2.0).min(h / 2.0);
    let cx = if x < r {
        r
    } else if x > w - r {
        w - r
    } else {
        return true;
    };
    let cy = if y < r {
        r
    } else if y > h - r {
        h - r
    } else {
        return true;
    };
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}
