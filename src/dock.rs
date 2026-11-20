//! 底部 Dock：钉选应用（.lnk）+ 运行中应用（按 exe 去重）+ 右侧托盘镜像。
//! 自绘图标与悬停底，点击钉选项启动、点击运行项切换窗口；托盘图标点击转发给
//! explorer 上真实的托盘图标（左键默认动作 / 右键在图标真实坐标发鼠标事件）。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::core::{w, Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_ALL, CLSCTX_INPROC_SERVER, STGM_READ};
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
    IShellDispatch4, IShellLinkW, SHGetFileInfoW, SHGetImageList, Shell, ShellExecuteW, ShellLink,
    SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_SYSICONINDEX, SHIL_JUMBO, SLGP_RAWPATH,
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
use crate::dockview::DockView;
use crate::notify;
use crate::render;
use crate::settings;
use crate::tray::TrayMirror;
use crate::flyout;
use crate::trayhost;
use crate::traymsg;

/// Dock 窗口的定时器 ID
const T_RESCAN: usize = 3;
/// 托盘闪烁采样（应用用一闪一闪提示新消息，跟随真实状态就得采得够快）
const T_FLASH: usize = 5;
/// 设置变化轮询（设置页面改值后宽度/托盘开关要立刻跟上）
const T_SETTINGS: usize = 6;
/// 后台扫完"图标回调消息"后叫醒窗口线程（lParam 不用，下标在 wParam）
const WM_TRAY_SCAN: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 0x71;

// ---------- 胶囊右侧的"托盘闪烁"快照 ----------
//
// 应用用托盘图标一闪一闪提示新消息（微信就是这样）。Dock 的 70ms 采样定时器
// 已经跟着真实闪烁走，这里把"正在闪烁的图标"发布成一份快照（图标像素 + 亮/灭帧
// + 回调目标 + 版本号），顶部胶囊按版本号跟随——胶囊右侧就会显示同一个图标、
// 以同样的节奏闪，点它等于点托盘图标。

/// 快照里的一条闪烁图标
#[derive(Clone)]
pub struct FlashIcon {
    /// 托盘图标的稳定键（`tray::TrayItem::key`）
    pub key: String,
    /// 显示名（tooltip 首行）
    pub name: String,
    /// 图标像素（straight RGBA，px*px*4）
    pub rgba: Vec<u8>,
    /// 位图边长
    pub px: i32,
    /// 这一帧是否"灭"（灭 = 图标消失帧，胶囊留空位不画）
    pub dim: bool,
    /// 回调目标（悬停/点击转发用；UIA 兜底模式可能还没有）。
    /// 存 `Raw` 而不是 `Target`：HWND 里是裸指针，静态快照要求 Send/Sync。
    pub target: Option<traymsg::Raw>,
}

static FLASH_SET: Mutex<Vec<FlashIcon>> = Mutex::new(Vec::new());
static FLASH_VER: AtomicU64 = AtomicU64::new(0);

/// 快照版本号：正在闪烁的图标集合（或亮/灭帧）变化时 +1
pub fn flash_version() -> u64 {
    FLASH_VER.load(Ordering::Relaxed)
}

/// 当前正在闪烁的图标（胶囊每帧取一次；量很小，直接克隆）
pub fn flash_icons() -> Vec<FlashIcon> {
    FLASH_SET.lock().map(|v| v.clone()).unwrap_or_default()
}

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
    /// egui 软件后端（Dock + 提示条绘制）
    view: DockView,
    scale: f32,
    items: Vec<DockItem>,
    hovered: Option<usize>,
    mouse_tracked: bool,
    /// 每个应用图标槽位 [x, y, size]（client 坐标）
    rects: Vec<[f32; 3]>,
    /// 每个托盘图标槽位 [x, y, size]（client 坐标）
    tray_rects: Vec<[f32; 3]>,
    /// 右端"返回桌面"细长条的命中矩形
    desktop_rect: Option<[f32; 4]>,
    hovered_desktop: bool,
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
    /// 上次见过的设置版本号（变了就重绘：托盘开关、返回桌面按钮都会改宽度）
    seen_settings: u64,
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
                // 就算没有新帧，也要把"闪烁停了"的图标从快照里过期掉
                dock.publish_flash();
                LRESULT(0)
            }
            WM_TIMER if w.0 == T_SETTINGS => {
                // 设置页面改了托盘开关 / 返回桌面按钮，宽度和内容都要重排
                if dock.seen_settings != settings::version() {
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
                let desk = dock.desktop_at(x as f32, y as f32);
                let idx = if desk { None } else { dock.index_at(x as f32, y as f32) };
                let tidx = if desk { None } else { dock.tray_index_at(x as f32, y as f32) };
                if idx != dock.hovered || tidx != dock.hovered_tray || desk != dock.hovered_desktop {
                    dock.hovered = idx;
                    dock.hovered_tray = tidx;
                    dock.hovered_desktop = desk;
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
                let had_desk = std::mem::replace(&mut dock.hovered_desktop, false);
                // 先清悬停态再转发：否则 forward_hover 会以为还停着，不发 leave
                dock.forward_hover();
                if had_app || had_tray || had_desk {
                    dock.render();
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                if dock.desktop_at(x as f32, y as f32) {
                    // 右端细长条：显示桌面 / 再点还原
                    crate::settingsui::hide();
                    dock.hide_tip();
                    dock.toggle_desktop();
                    return LRESULT(0);
                }
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
                // 托盘图标之外（Dock 空白处 / 返回桌面细长条）右键 = 打开设置页面
                if dock.pending_right.is_none() {
                    dock.hide_tip();
                    crate::settingsui::toggle();
                }
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
            view: DockView::new(),
            scale: 1.0,
            items: Vec::new(),
            hovered: None,
            mouse_tracked: false,
            rects: Vec::new(),
            tray_rects: Vec::new(),
            desktop_rect: None,
            hovered_desktop: false,
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
            seen_settings: 0,
        }
    }

    pub fn init(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        self.scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
        self.taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        self.seen_settings = settings::version();
        self.tip = unsafe { create_tip_window() };
        self.tray.attach_host();
        self.scan();
        self.poll_tray();
        self.render();
    }

    /// 轮询托盘镜像；返回是否有变化（需要重绘）
    fn poll_tray(&mut self) -> bool {
        if !settings::show_tray() {
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
        if !settings::show_tray() {
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
                // 按图标**真实注册的协议版本**发消息：
                //   v4 → wParam = 屏幕坐标、lParam = (uid<<16)|事件
                //   v0/v3 → wParam = uid、lParam = 事件
                // 以前这里一律当成 v4，v0 的应用（火绒这类没调 NIM_SETVERSION 的）
                // 会把坐标当 uid，左右键和悬停全部没反应；微信注册的就是 v4，走的还是同一条路。
                let ver = match h.ver {
                    3 => 3,
                    4 => 4,
                    _ => 0,
                };
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
        crate::vlog!(
            "[dock] 回调目标重建：{}",
            v.iter()
                .zip(self.tray.items.iter())
                .map(|(t, it)| match t {
                    Some(t) => format!("{}v{}(msg={:#x})", short_exe(&it.exe), t.ver, t.msg),
                    None => format!("{}无", short_exe(&it.exe)),
                })
                .collect::<Vec<_>>()
                .join(", ")
        );
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
        // 弹窗要"顶在鼠标所在的那个图标上方"，不是屏幕正中间。
        // 锚点用图标中心（鼠标正停在这个图标上），不用实时光标：悬停会被反复重发，
        // 用光标的话鼠标在图标里微动一下卡片就跟着左右滑。
        // Dock 靠上时卡片改摆到 Dock **下方**（上面出屏）。
        let anchor_x = self.tray_center(idx).0;
        let below = settings::dock_anchor() == crate::settings::DockAnchor::Top;
        flyout::watch(t.pid, t.hwnd, cap, anchor_x, &label, below);
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
        // 先定"哪些托盘图标要显示"（只滤设置里单独隐藏的），
        // 再把和托盘同程序的运行项从 Dock 应用位藏掉：任务栏图标隐藏、托盘图标保留
        self.refresh_tray_shown();
        self.hide_dock_duplicates();
        self.seen_settings = settings::version();
        let show_tray = settings::show_tray();
        let show_desktop = settings::show_desktop_btn();
        let n = self.items.len();
        let n_tray = if show_tray { self.tray_shown.len() } else { 0 };
        // 靠哪条边：下/上是水平条，左/右是竖排条
        let anchor = settings::dock_anchor();
        let vertical = anchor.is_vertical();
        let (lw, lh) = crate::dockview::dock_size(n, n_tray, show_tray, show_desktop, vertical);
        let w = (lw * self.scale).ceil() as i32;
        let h = (lh * self.scale).ceil() as i32;
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
        let out = self.view.paint(
            w, h, self.scale, &icons, self.hovered, &tray_icons, self.hovered_tray,
            show_tray, show_desktop, self.hovered_desktop, vertical,
        );
        self.rects = out.rects.clone();
        self.tray_rects = out.tray_rects.clone();
        self.desktop_rect = out.desktop_rect;
        // egui 软件后端输出已是预乘 RGBA，直接换 BGRA 上屏
        let bgra = render::premultiplied_rgba_to_bgra(&out.rgba);
        render::present(self.hwnd, &bgra, w, h);
        crate::vlog!(
            "[dock] anchor={:?} w={w} h={h} apps={} tray={}/{} desktop={:?} rects={:?} tray_rects={:?} hovered_tray={:?}",
            anchor, self.items.len(), n_tray, self.tray.items.len(), self.desktop_rect, self.rects,
            self.tray_rects, self.hovered_tray
        );

        // 定位：沿 dock_anchor 贴对应的屏幕边。
        // 水平（下/上）按"屏幕矩形"居中；垂直（左/右）竖向居中。
        let mi = crate::window::primary_monitor();
        let bottom = if crate::hide::is_active() { mi.rcMonitor.bottom } else { mi.rcWork.bottom };
        let mon = mi.rcMonitor;
        let (x, y) = match anchor {
            crate::settings::DockAnchor::Bottom => (
                mon.left + (mon.right - mon.left - w) / 2,
                bottom - h - (cfg::DOCK_BOTTOM_MARGIN as f32 * self.scale) as i32,
            ),
            crate::settings::DockAnchor::Top => (mon.left + (mon.right - mon.left - w) / 2, mon.top),
            crate::settings::DockAnchor::Left => (mon.left, mon.top + (mon.bottom - mon.top - h) / 2),
            crate::settings::DockAnchor::Right => (mon.right - w, mon.top + (mon.bottom - mon.top - h) / 2),
        };
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

        // 托盘成员/目标变了，闪烁快照跟着刷新（70ms 定时器里也会再发一次）
        self.publish_flash();
    }

    /// 把"正在闪烁的托盘图标"发布成胶囊消费的快照（见 `FlashIcon`）。
    /// 集合或亮/灭帧没变就不动版本号，胶囊那边靠版本号判断要不要重绘。
    fn publish_flash(&mut self) {
        let now = Instant::now();
        // 微信提示音：拿"上一轮快照里在闪的键"对比，图标从"没在闪"变"在闪"
        // 的那一下响一声（一轮提醒只响一次；亮灭交替不会反复触发）
        let was_flashing: HashSet<String> = FLASH_SET
            .lock()
            .map(|v| v.iter().map(|f| f.key.clone()).collect())
            .unwrap_or_default();
        let mut wx_new = false;
        let mut snap: Vec<FlashIcon> = Vec::new();
        if settings::show_tray() {
            for &i in &self.tray_shown {
                let Some(it) = self.tray.items.get(i) else { continue };
                let Some(t) = it.flash_at else { continue };
                if now.duration_since(t).as_millis() as u64 > cfg::FLASH_WINDOW_MS {
                    continue;
                }
                let key = it.key();
                if settings::wechat_sound()
                    && !was_flashing.contains(&key)
                    && !wx_new
                    && notify::is_wechat(&it.name, &it.exe)
                {
                    wx_new = true;
                }
                let px = ((it.rgba.len() / 4) as f32).sqrt().round() as i32;
                snap.push(FlashIcon {
                    key,
                    name: it.name.clone(),
                    rgba: it.rgba.clone(),
                    px: if px > 0 { px } else { 0 },
                    dim: it.dim,
                    target: self.host_targets.get(i).cloned().flatten().map(|t| t.raw()),
                });
            }
        }
        if wx_new {
            notify::play_wechat();
        }
        let sig: Vec<(String, bool)> = snap.iter().map(|f| (f.key.clone(), f.dim)).collect();
        let mut changed = true;
        if let Ok(cur) = FLASH_SET.lock() {
            changed = cur.iter().map(|f| (f.key.clone(), f.dim)).collect::<Vec<_>>() != sig;
        }
        if changed {
            if let Ok(mut cur) = FLASH_SET.lock() {
                *cur = snap;
            }
            FLASH_VER.fetch_add(1, Ordering::Relaxed);
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

    /// 托盘显示集合：只滤掉设置页里单独关掉的图标（`hide_tray`），
    /// 其余全部显示。托盘图标**一律不因与 Dock 重复而隐藏**——重复的程序
    /// 反过来从 Dock 应用位藏掉（见 `hide_dock_duplicates`）。
    fn refresh_tray_shown(&mut self) {
        let n = self.tray.items.len();
        if !settings::show_tray() {
            self.tray_shown.clear();
            return;
        }
        let hidden = settings::hidden_tray();
        self.tray_shown = (0..n)
            .filter(|&i| {
                self.tray
                    .items
                    .get(i)
                    .map_or(false, |it| !hidden.iter().any(|k| *k == it.key()))
            })
            .collect();
    }

    /// 同一个进程的图标同时出现在托盘和 Dock 应用位时：隐藏"任务栏图标"，
    /// 托盘图标保留。托盘图标被设置页单独隐藏的（不在 tray_shown 里）不算，
    /// Dock 图标照常显示。explorer 托管的图标（exe 是 explorer.exe）拿
    /// tooltip 名字到 Dock 项里兜底匹配。
    fn hide_dock_duplicates(&mut self) {
        let tray_exes: HashSet<String> = self
            .tray_shown
            .iter()
            .filter_map(|&i| self.tray.items.get(i))
            .filter(|it| !it.exe.is_empty() && !it.exe.ends_with("explorer.exe"))
            .map(|it| it.exe.to_lowercase())
            .collect();
        let tray_names: Vec<String> = self
            .tray_shown
            .iter()
            .filter_map(|&i| self.tray.items.get(i))
            .filter(|it| it.exe.is_empty() || it.exe.ends_with("explorer.exe"))
            .map(|it| normalize_name(&it.name))
            .filter(|n| n.len() >= 3)
            .collect();
        if tray_exes.is_empty() && tray_names.is_empty() {
            return;
        }
        let before = self.items.len();
        self.items.retain(|it| {
            if !it.running {
                return true;
            }
            if !it.exe.is_empty() && tray_exes.contains(&it.exe) {
                return false;
            }
            if !it.norm.is_empty()
                && tray_names.iter().any(|n| it.norm.contains(n) || n.contains(&it.norm))
            {
                return false;
            }
            true
        });
        if self.items.len() != before {
            crate::vlog!(
                "[dock] 托盘同程序：隐藏 {} 个 Dock 应用图标（{} → {}）",
                before - self.items.len(),
                before,
                self.items.len()
            );
        }
        if self.hovered.map(|i| i >= self.items.len()).unwrap_or(false) {
            self.hovered = None;
        }
    }

    /// 根据当前悬停项显示提示条（托盘用 tooltip 名字，任务图标用程序名）
    fn update_tip(&mut self) {
        // 末端细长条：按钮本身没有文字，靠提示条说清楚它是干什么的
        if self.hovered_desktop {
            match self.desktop_rect {
                Some(r) => {
                    let mut wr = RECT::default();
                    unsafe {
                        let _ = GetWindowRect(self.hwnd, &mut wr);
                    }
                    let cx = wr.left + (r[0] + r[2] / 2.0) as i32;
                    let cy = wr.top + (r[1] + r[3] / 2.0) as i32;
                    let ty = wr.top + r[1] as i32;
                    self.show_tip("返回桌面", cx, cy, ty);
                }
                None => self.hide_tip(),
            }
            return;
        }
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
        let cy = wr.top + (rect[1] + rect[2] / 2.0) as i32;
        let ty = wr.top + rect[1] as i32;
        self.show_tip(&text, cx, cy, ty);
    }

    /// 提示条位置随 Dock 锚位走：下 = 图标上方；上 = Dock 下方；
    /// 左 = Dock 右侧；右 = Dock 左侧。文本在锚点方向上居中。
    fn show_tip(&mut self, text: &str, cx: i32, cy: i32, ty: i32) {
        if self.tip.is_invalid() {
            crate::vlog!("[dock] 提示条窗口无效，跳过");
            return;
        }
        let (w, h, rgba) = self.view.paint_tooltip(text, self.scale);
        if w <= 0 || h <= 0 {
            crate::vlog!("[dock] 提示条绘制失败：{text}");
            return;
        }
        let bgra = render::premultiplied_rgba_to_bgra(&rgba);
        render::present(self.tip, &bgra, w, h);
        let mon = crate::window::primary_monitor().rcMonitor;
        let gap = (cfg::TIP_GAP as f32 * self.scale) as i32;
        let clamp_x = |x: i32| x.clamp(mon.left + 4, (mon.right - w - 4).max(mon.left + 4));
        let clamp_y = |y: i32| y.clamp(mon.top + 4, (mon.bottom - h - 4).max(mon.top + 4));
        let mut wr = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut wr);
        }
        let (x, y) = match settings::dock_anchor() {
            crate::settings::DockAnchor::Bottom => (clamp_x(cx - w / 2), ty - h - gap),
            crate::settings::DockAnchor::Top => (clamp_x(cx - w / 2), wr.bottom + gap),
            crate::settings::DockAnchor::Left => (wr.right + gap, clamp_y(cy - h / 2)),
            crate::settings::DockAnchor::Right => (wr.left - w - gap, clamp_y(cy - h / 2)),
        };
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

    /// 是否点在右端"返回桌面"细长条上（命中区比细条宽一圈）
    fn desktop_at(&self, x: f32, y: f32) -> bool {
        self.desktop_rect
            .map_or(false, |r| x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3])
    }

    /// 显示桌面 / 还原（等价 Win+D）。用 Shell 的 ToggleDesktop，
    /// 桌面图标也会跟着收起/展开，和系统行为一致；自身窗口不受影响。
    fn toggle_desktop(&self) {
        unsafe {
            match CoCreateInstance::<_, IShellDispatch4>(&Shell, None, CLSCTX_ALL) {
                Ok(d) => {
                    if let Err(e) = d.ToggleDesktop() {
                        crate::tlog!("[dock] ToggleDesktop 失败：{e}");
                    } else {
                        crate::vlog!("[dock] 返回桌面（ToggleDesktop）");
                    }
                }
                Err(e) => crate::tlog!("[dock] Shell.Application 创建失败：{e}"),
            }
        }
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

/// exe 全路径取文件名（日志用）
fn short_exe(exe: &str) -> &str {
    exe.rsplit(['\\', '/']).next().unwrap_or(exe)
}
