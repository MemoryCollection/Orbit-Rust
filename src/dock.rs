//! 底部 Dock：钉选应用（.lnk）+ 运行中应用（按 exe 去重）+ 右侧托盘镜像。
//! 自绘图标与悬停底，点击钉选项启动、点击运行项切换窗口；托盘图标点击转发给
//! explorer 上真实的托盘图标（左键默认动作 / 右键在图标真实坐标发鼠标事件）。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use windows::core::{w, Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, IShellLinkW, SHCreateItemFromIDList, SHGetFileInfoW, SHGetImageList,
    SHParseDisplayName, ShellExecuteW, ShellLink, SIIGBF, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
    SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_SYSICONINDEX, SHIL_JUMBO, SLGP_RAWPATH, ILFree,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::PropertiesSystem::{
    IPropertyStore, PROPERTYKEY, SHGetPropertyStoreForWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, EnumWindows, GetClassNameW, GetCursorPos,
    GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, HICON, HMENU, IDC_ARROW, IsWindowVisible, LoadCursorW,
    LoadImageW, PostMessageW, RegisterClassW, RegisterWindowMessageW, CREATESTRUCTW, IMAGE_ICON,
    LR_LOADFROMFILE, WNDCLASSW,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, GWLP_USERDATA,
    GW_OWNER, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, SWP_NOACTIVATE, SW_HIDE,
    SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WM_CREATE, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCHITTEST, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WS_EX_LAYERED,
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
    /// 这一帧是否"灭"（灭 = 图标消失帧，胶囊留空位不画）
    pub dim: bool,
    /// 回调目标（悬停/点击转发用；UIA 兜底模式可能还没有）。
    /// 存 `Raw` 而不是 `Target`：HWND 里是裸指针，静态快照要求 Send/Sync。
    pub target: Option<traymsg::Raw>,
}

static FLASH_SET: Mutex<Vec<FlashIcon>> = Mutex::new(Vec::new());
static FLASH_VER: AtomicU64 = AtomicU64::new(0);
/// 每个图标 key 上次**见到**的闪烁时刻（`it.flash_at`）。
/// 提示音按"闪烁轮次"判定：同一轮连续闪烁里相邻两闪只差约 500ms
/// （小于 `FLASH_SOUND_GAP_MS`），不重复响；停止闪烁 1 秒后再闪，
/// 间隔 ≥ 1 秒，视为新一轮，再响一声。之前按"响声时刻"节流，
/// 消息一直不读会每秒响一声（太吵），改成整轮只响开头一声。
static SOUND_FLASH_AT: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// 判定"新一轮闪烁"的最小间隔（毫秒）：距上次闪烁 ≥ 1 秒才再响
const FLASH_SOUND_GAP_MS: u128 = 1000;
/// pid → 托盘回调目标（build_host_targets 时同步）。横幅点击跳转兜底用：
/// 微信主窗口收在托盘（不可见）时，SetForegroundWindow 无效，
/// 唯一可靠的唤醒方式是模拟左键它的托盘图标。
static TRAY_BY_PID: LazyLock<Mutex<HashMap<u32, traymsg::Raw>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 模拟左键点击某个 pid 的托盘图标（走图标注册的回调消息，不动光标）。
/// 横幅点击跳转兜底：微信窗口收在托盘时用它把主窗口唤出来。
pub fn click_tray_for_pid(pid: u32) {
    if !settings::tray_fake_input() {
        return; // 透明模式：不伪造托盘点击
    }
    let raw = TRAY_BY_PID.lock().ok().and_then(|m| m.get(&pid).copied());
    let Some(raw) = raw else {
        crate::tlog!("[dock] 横幅兜底：没有 pid={pid} 的托盘回调目标");
        return;
    };
    let t = traymsg::Target::from_raw(raw);
    let c = traymsg::icon_center_of(t).unwrap_or((0, 0));
    crate::tlog!("[dock] 横幅兜底：模拟左键 pid={pid} 的托盘图标（回调消息投递）");
    traymsg::click(&t, c, false);
}

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
    /// 该程序当前所有可见窗口（前台窗口排第一）。
    /// 空 = 没在运行；1 个 = 直接切过去；多个 = 左键弹窗让用户挑。
    /// 钉选项也带这份列表：程序已经在跑时点图标应该切窗口，而不是再启动一个
    wins: Vec<HWND>,
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
    /// 前台应用在 `items` 里的下标（图标下面那条指示条用）
    active: Option<usize>,
    /// 前台窗口属于哪个托盘图标（`tray_shown` 的下标）：托盘程序在最前也要有指示条
    active_tray: Option<usize>,
    /// 上次查前台应用的时刻（70ms 定时器里节流，别每帧都去 OpenProcess）
    active_checked: Option<Instant>,
    /// 用户"指定了图标"的托盘项像素（键 → 预乘外的 straight RGBA）。
    /// 只有本帧要画的托盘项才在这里，避免每帧都去 exe 里抠图标
    over_px: Vec<(String, Vec<u8>)>,
    /// 上次构建 `over_px` 的 (设置版本, 图标像素边长)
    over_key: (u64, i32),
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
    /// 底板在窗口客户区里的矩形 [x, y, w, h]（物理像素）。
    /// 窗口在内侧多一圈透明留白装"伸出去"的悬停图标，命中测试/提示条/弹窗
    /// 都要对齐底板矩形，而不是整窗矩形（留白区必须保持点击穿透）。
    board: (i32, i32, i32, i32),
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
    /// 上次绘制用的图标缩放（变了要把应用图标 / 托盘像素按新尺寸重抓）
    icon_scale: f32,
    /// 托盘表"变成空"的起始时刻：应用重注册图标时 `NOTIFYICONDATA` 表会短暂为空，
    /// 这时要是直接按 0 个托盘图标算尺寸，Dock 会瞬间塌掉半截再长回来
    tray_empty_since: Option<Instant>,

    // ---- 同程序多开的窗口选择弹窗 ----
    /// 弹窗窗口句柄（ORBIT_WINLIST，独立分层窗）
    winlist: HWND,
    /// 弹窗里的一行一个窗口：(句柄, 标题)
    winlist_rows: Vec<(HWND, String)>,
    /// 鼠标悬停行
    winlist_hover: Option<usize>,
    /// 每行命中矩形（物理像素 client 坐标）
    winlist_rects: Vec<[f32; 4]>,
    /// 弹窗尺寸
    winlist_size: (i32, i32),
    /// 鼠标离开弹窗的那一刻（超过 `WINLIST_GRACE_MS` 就收起）
    winlist_left: Option<Instant>,
    /// 光标在 Dock 窗口内的位置（逻辑点），驱动图标的波浪放大；离开 Dock 置 None
    mouse: Option<(f32, f32)>,
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
                // 顺带跟随前台应用：变了就重画（指示条）。查一次要走 GetForegroundWindow
                // 和可能的 OpenProcess，节流到 ~300ms，Alt-Tab 后仍然是"立刻"跟上的观感
                let due = dock
                    .active_checked
                    .map(|t| t.elapsed().as_millis() >= 300)
                    .unwrap_or(true);
                if due {
                    dock.active_checked = Some(Instant::now());
                    if dock.refresh_active() {
                        dock.render();
                    }
                }
                if dock.poll_flash() {
                    dock.render();
                }
                // 弹窗的自动收起也搭这趟车（鼠标离开 450ms），不另开定时器
                if dock.winlist_tick() {
                    // 收起本身不需要重绘 Dock
                }
                // 就算没有新帧，也要把"闪烁停了"的图标从快照里过期掉
                dock.publish_flash();
                LRESULT(0)
            }
            WM_TIMER if w.0 == T_SETTINGS => {
                // 设置页面改了托盘开关 / 返回桌面按钮，宽度和内容都要重排
                if dock.seen_settings != settings::version() {
                    dock.apply_settings_change();
                }
                LRESULT(0)
            }
            // 设置页点「应用/还原」后主动投来：立即重扫+重绘，不等 500ms 轮询
            m if m == crate::appmsg::WM_SETTINGS_APPLIED => {
                // 设置页在独立子进程里写盘，本进程内存还是旧值，先重读再重排
                settings::reload_if_changed();
                dock.apply_settings_change();
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
                // 命中区 = 底板矩形（窗口内侧留白是透明的，必须穿透）。
                // 例外：悬停放大的图标会冒出底板，落在留白里的那半截视觉上是
                // 图标本体——点它也要能选中，所以弹出矩形同样算命中区。
                let (bx, by, bw, bh) = dock.board;
                let hit = bw > 0
                    && inside_round(
                        (pt.x - bx) as f32,
                        (pt.y - by) as f32,
                        bw as f32,
                        bh as f32,
                        // 与 dockview 的底板圆角同一口径：统一设置 + 主题上限 + 高度一半
                        (crate::settings::corner_radius()
                            .min(cfg::cur().dock_radius)
                            .max(0.0))
                            * dock.scale
                            * settings::icon_scale(),
                    );
                let hit = hit || {
                    let (app, tray) = dock.popped_rects();
                    let (px, py) = (pt.x as f32, pt.y as f32);
                    app.iter()
                        .chain(tray.iter())
                        .any(|r| {
                            px >= r[0] && px <= r[0] + r[2]
                                && py >= r[1] && py <= r[1] + r[3]
                        })
                };
                if hit {
                    LRESULT(HTCLIENT as isize)
                } else {
                    LRESULT(HTTRANSPARENT as isize)
                }
            }
            WM_MOUSEMOVE => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                // 记录光标位置：命中用物理像素，波浪放大用逻辑点（和 paint 同坐标系）
                let (fx, fy) = (x as f32, y as f32);
                let k = (dock.scale * crate::settings::icon_scale().clamp(0.5, 2.0)).max(0.5);
                let new_mouse = Some((fx / k, fy / k));
                let mouse_moved = new_mouse != dock.mouse;
                dock.mouse = new_mouse;
                let desk = dock.desktop_at(fx, fy);
                let idx = if desk { None } else { dock.index_at(fx, fy) };
                let tidx = if desk { None } else { dock.tray_index_at(fx, fy) };
                if idx != dock.hovered || tidx != dock.hovered_tray || desk != dock.hovered_desktop {
                    dock.hovered = idx;
                    dock.hovered_tray = tidx;
                    dock.hovered_desktop = desk;
                    dock.render();
                    // 悬停即显示提示条（托盘名字来自 UIA，任务图标用程序名）
                    dock.update_tip();
                    dock.forward_hover();
                } else if mouse_moved {
                    // 悬停目标没变但光标在动：波浪放大要跟随光标，逐帧重绘
                    dock.render();
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
                // 波浪放大跟着这个坐标走，必须清掉并重绘；哪怕光标停在 Dock 空白处
                // （悬停目标本来就是 None），不重绘的话放大的图标会冻结在原位
                let had_mouse = dock.mouse.take().is_some();
                dock.hide_tip();
                // 注意不能写成 `a.take().is_some() || b.take().is_some()`：
                // 短路后第二个 take 不会执行，托盘悬停状态会卡住
                let had_app = dock.hovered.take().is_some();
                let had_tray = dock.hovered_tray.take().is_some();
                let had_desk = std::mem::replace(&mut dock.hovered_desktop, false);
                // 先清悬停态再转发：否则 forward_hover 会以为还停着，不发 leave
                dock.forward_hover();
                if had_app || had_tray || had_desk || had_mouse {
                    dock.render();
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                // 任何一次点在 Dock 上都把窗口列表收掉（点图标 = 换目标，点空白 = 取消）
                let had_winlist = dock.winlist_is_open();
                dock.hide_winlist();
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
                    // 任务管理器的 CPU 图标是 elevated 进程：回调消息投不进去、窗口
                    // 也拉不动（详见 traymsg::wake_taskmgr），只能走"再启动一次"。
                    let taskmgr = dock
                        .tray_shown
                        .get(idx)
                        .and_then(|&real| dock.tray.items.get(real))
                        .map_or(false, |it| traymsg::is_taskmgr(&it.exe, &it.name));
                    if let Some(t) = dock.tray_targets.get(&c).copied() {
                        crate::vlog!("[dock] 左键托盘#{idx}（回调消息{}）", if dbl { " 双击" } else { "" });
                        // 点击 = 悬停结束，应用要开真面板了：先吊销守位线程，别让它碰真 UI
                        flyout::cancel(t.hwnd, t.pid);
                        if settings::tray_fake_input() {
                            traymsg::click(&t, c, dbl);
                        } else if let Some(&real) = dock.tray_shown.get(idx) {
                            // 透明模式：不伪造回调消息，退回 UIA 默认动作（让 Explorer 处理）
                            dock.tray.click(real, false);
                        }
                    } else if let Some(&real) = dock.tray_shown.get(idx) {
                        crate::vlog!("[dock] 左键托盘 显示#{idx} → 实际#{real}（UIA）");
                        dock.tray.click(real, false);
                    }
                    if taskmgr {
                        traymsg::wake_taskmgr();
                    }
                } else if let Some(idx) = dock.index_at(x as f32, y as f32) {
                    dock.activate(idx);
                } else if had_winlist {
                    // 点在 Dock 空白处：只是收起列表，不做别的
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
                            // 同左键：右键弹出的菜单也是应用的真 UI，悬停守位线程必须先停
                            flyout::cancel(t.hwnd, t.pid);
                            if settings::tray_fake_input() {
                                // 但菜单本身会弹在注入坐标（= Dock 图标上），被 topmost 的
                                // Dock 压住一角——改由菜单守位线程把它挪到 Dock 旁边
                                if cfg::TRAY_RELOCATE_MENU {
                                    let mut cap = RECT::default();
                                    if GetWindowRect(hwnd, &mut cap).is_ok() {
                                        let label = dock.flyout_label(idx);
                                        flyout::watch_menu(t.pid, t.hwnd, cap, c, &label);
                                    }
                                }
                                traymsg::right_click(&t, c);
                            } else if let Some(&real) = dock.tray_shown.get(idx) {
                                // 透明模式：退回 UIA 真实坐标右键
                                dock.tray.click(real, true);
                            }
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
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                // 分辨率/系统缩放变化：重读 DPI 再重绘（图标像素、槽位、窗口尺寸都跟着走）
                dock.scale = GetDpiForWindow(hwnd) as f32 / 96.0;
                // 托盘图标像素按新边长立刻重采一次，别等 2s 的重扫定时器
                dock.poll_tray();
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
            active: None,
            active_tray: None,
            active_checked: None,
            over_px: Vec::new(),
            over_key: (0, 0),
            mouse_tracked: false,
            rects: Vec::new(),
            tray_rects: Vec::new(),
            desktop_rect: None,
            hovered_desktop: false,
            tray: TrayMirror::default(),
            tray_shown: Vec::new(),
            hovered_tray: None,
            size: (0, 0),
            board: (0, 0, 0, 0),
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
            icon_scale: 1.0,
            tray_empty_since: None,
            winlist: HWND::default(),
            winlist_rows: Vec::new(),
            winlist_hover: None,
            winlist_rects: Vec::new(),
            winlist_size: (0, 0),
            winlist_left: None,
            mouse: None,
        }
    }

    pub fn init(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        self.scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
        self.taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        self.seen_settings = settings::version();
        // 登记 HWND：设置页点「应用」后主动投消息过来，立即重扫+重绘（不用等轮询）
        crate::appmsg::register_dock(hwnd);
        self.tip = unsafe { create_tip_window() };
        // 弹窗的窗口过程要拿回 Dock 实例：把 self 的指针当 lpCreateParams 传进去
        self.winlist = unsafe { create_winlist_window(self as *mut Dock) };
        self.tray.attach_host();
        self.scan();
        self.refresh_active();
        self.poll_tray();
        self.render();
    }

    /// 设置变更后重排：图标大小、固定列表、托盘开关、返回桌面按钮都可能变。
    /// 定时器轮询和设置页主动通知共用这一条路径。
    fn apply_settings_change(&mut self) {
        self.seen_settings = settings::version();
        // 图标大小变了：应用图标与托盘像素都要按新尺寸重抓（否则放大会糊）
        let k = settings::icon_scale();
        if (k - self.icon_scale).abs() > 1e-3 {
            self.icon_scale = k;
            self.tray.invalidate();
        }
        // 固定图标列表 / 开关都可能变，内容整轮重扫（签名没变时等价于空转）
        self.scan();
        self.poll_tray();
        self.refresh_active();
        self.render();
    }

    /// 轮询托盘镜像；返回是否有变化（需要重绘）
    fn poll_tray(&mut self) -> bool {
        if !settings::show_tray() {
            return false;
        }
        let px = (cfg::DOCK_TRAY_ICON * self.scale * settings::icon_scale()).round() as i32;
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
        let px = (cfg::DOCK_TRAY_ICON * self.scale * settings::icon_scale()).round() as i32;
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
        // 同步一份 pid → 回调目标：给横幅跳转兜底用（微信收在托盘时模拟左键）
        if let Ok(mut m) = TRAY_BY_PID.lock() {
            m.clear();
            for t in v.iter().flatten() {
                m.insert(t.pid, t.raw());
            }
        }
        self.host_targets = v;
    }

    /// 悬停态变化时把"指针在图标上/离开"转发给真实托盘图标。
    /// 走的是图标注册时的回调消息（wParam/lParam 按协议版本编码），**不动光标**，
    /// 也不用管任务栏是否可见——这也正是"悬停即弹卡片"能成立的前提。
    fn forward_hover(&mut self) {
        if !cfg::TRAY_FORWARD_HOVER || !settings::tray_fake_input() {
            // 透明模式（伪造输入关闭）：不发悬停回调；清掉上一轮状态
            if !settings::tray_fake_input() {
                self.hover_sent = None;
                self.last_hover_move = None;
            }
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

    /// 守位日志用的图标名（托盘显示下标 → 真实条目名）
    fn flyout_label(&self, idx: usize) -> String {
        self.tray_shown
            .get(idx)
            .and_then(|&real| self.tray.items.get(real))
            .map(|it| it.name.clone())
            .unwrap_or_default()
    }

    /// 应用在悬停时会自己弹一个小预览窗，位置在旧托盘角；把它摆到 Dock 上方。
    /// 真正的活儿在后台线程里（`flyout::watch`），这里只把 Dock 的屏幕矩形递过去。
    fn watch_flyout(&self, t: traymsg::Target, idx: usize) {
        if !cfg::TRAY_RELOCATE_FLYOUT {
            return;
        }
        let label = self.flyout_label(idx);
        let mut cap = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut cap) }.is_err() {
            return;
        }
        // 弹窗要"顶在鼠标所在的那个图标上方"，不是屏幕正中间。
        // 锚点用图标中心（鼠标正停在这个图标上），不用实时光标：悬停会被反复重发，
        // 用光标的话鼠标在图标里微动一下卡片就跟着左右滑。
        // Dock 靠上时卡片改摆到 Dock **下方**（上面出屏）。
        let anchor = self.tray_center(idx);
        let below = settings::dock_anchor() == crate::settings::DockAnchor::Top;
        flyout::watch(t.pid, t.hwnd, cap, anchor, &label, below);
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
        let px = (cfg::DOCK_ICON * self.scale * settings::icon_scale()).round() as i32;
        let mut items: Vec<DockItem> = Vec::new();
        // 有可见窗口的应用（exe 小写 → 该程序的全部窗口，前台优先）；
        // 托盘去重要靠它判断"程序是否已在 Dock 上"
        let groups = running_groups();
        let run_map: BTreeMap<String, HWND> = groups
            .iter()
            .filter_map(|(exe, w)| w.first().map(|h| (exe.clone(), *h)))
            .collect();

        // 钉选应用（顺序按文件名）
        let mut pinned_exes: Vec<String> = Vec::new();
        // 解析不出目标的钉选项（CLSID 快捷方式，如 File Explorer）靠名字匹配运行项
        let mut pinned_names: Vec<String> = Vec::new();
        for pin in effective_pins() {
            // 设置文件里手写 / 从别处粘来的路径可能是正斜杠：Win32 取图标、比对 exe
            // 都只认反斜杠，这里统一转一遍（ShellExecuteW 两种都吃，转完更保险）
            let pin = pin.replace('/', "\\");
            let lnk = PathBuf::from(&pin);
            let is_lnk = lnk
                .extension()
                .map(|e| e.eq_ignore_ascii_case("lnk"))
                .unwrap_or(false);
            let target = if is_lnk {
                shell_link_target(&lnk).map(|t| expand_env(&t)).unwrap_or_default()
            } else {
                pin.clone()
            };
            let name = lnk.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let rgba = if is_lnk {
                pinned_icon(&lnk, &target, px)
            } else {
                large_icon_of_path(&lnk, px)
            };
            // 认不出图标（路径失效 / 权限）就别占一个空格子：留着只会让 Dock 左边空一块
            if rgba.is_empty() {
                crate::tlog!("[dock] 固定项 {pin} 取不到图标，跳过");
                continue;
            }
            if target.is_empty() {
                let n = normalize_name(&name);
                if n.len() >= 8 {
                    pinned_names.push(n);
                }
            }
            let exe = target.to_lowercase();
            let norm = normalize_name(&name);
            pinned_exes.push(exe.clone());
            let wins = groups.get(&exe).cloned().unwrap_or_default();
            items.push(DockItem {
                tip: name.clone(),
                name,
                norm,
                running: !wins.is_empty(),
                exe,
                target: Target::Lnk(lnk),
                wins,
                rgba,
            });
        }

        // 运行中应用（exe / AUMID 去重，已钉选的跳过）
        for (key, hwnd) in &run_map {
            if pinned_exes.iter().any(|p| !p.is_empty() && p == key) {
                continue;
            }
            let uwp = key.strip_prefix(UWP_PREFIX);
            // UWP 的"exe"是宿主 ApplicationFrameHost，拿它当名字只会出现一堆同名项，
            // 用窗口标题（「设置」「照片」）当名字更接近用户在系统里看到的东西
            let name = match uwp {
                Some(_) => window_title(*hwnd).unwrap_or_else(|| key.clone()),
                None => PathBuf::from(key)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
            };
            let norm = normalize_name(&name);
            if pinned_names.iter().any(|p| norm.contains(p.as_str()) || p.contains(&norm)) {
                continue;
            }
            // 大图标：UWP 走 AppsFolder 的 App logo，其余从 exe 取；都没有再退回窗口图标
            let mut rgba = match uwp {
                Some(aumid) => aumid_icon(aumid, px),
                None => large_icon_of_path(std::path::Path::new(key), px),
            };
            if rgba.is_empty() {
                if let Some(exe) = exe_path(*hwnd) {
                    rgba = large_icon_of_path(std::path::Path::new(&exe), px);
                }
            }
            if rgba.is_empty() {
                rgba = unsafe { render::window_icon(*hwnd) }
                    .and_then(|h| render::capture_icon_rgba(h, px))
                    .unwrap_or_default();
            }
            let wins = groups.get(key).cloned().unwrap_or_else(|| vec![*hwnd]);
            // 多开时提示条写"名字 + 窗口数"，用户才知道点一下会弹列表
            let tip = if wins.len() > 1 {
                format!("{}（{} 个窗口）", name, wins.len())
            } else {
                window_title(*hwnd).unwrap_or_else(|| name.clone())
            };
            items.push(DockItem {
                tip,
                name,
                norm,
                exe: key.clone(),
                running: true,
                target: Target::Window(*hwnd),
                wins,
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
        // 列表重排后按下标记的 `active` 会指到别的图标上：跟着重算一次，
        // 别让指示条在两个图标之间错位（T_FLASH 的 300ms 节流太慢，扫完就得对上）
        self.active = self.foreground_hits().0;
        changed
    }

    pub fn render(&mut self) {
        // 先定"哪些托盘图标要显示"（只滤设置里单独隐藏的），
        // 再把和托盘同程序的运行项从 Dock 应用位藏掉：任务栏图标隐藏、托盘图标保留
        self.refresh_tray_shown();
        self.hide_dock_duplicates();
        self.seen_settings = settings::version();
        // 托盘图标允许在设置页"指定图标"（源 exe 的 256px 大图标）。
        // 缓存必须在构造 icons/tray_icons 的借用之前刷新
        let tray_px = (cfg::DOCK_TRAY_ICON * self.scale * settings::icon_scale()).round() as i32;
        self.refresh_icon_over(tray_px);
        let show_tray = settings::show_tray();
        let show_desktop = settings::show_desktop_btn();
        let n = self.items.len();
        let n_tray = if show_tray { self.tray_shown.len() } else { 0 };
        // 靠哪条边：下/上是水平条，左/右是竖排条
        let anchor = settings::dock_anchor();
        let vertical = anchor.is_vertical();
        let (lw, lh) = crate::dockview::dock_size(n, n_tray, show_tray, show_desktop, vertical);
        let bw = (lw * self.scale).ceil() as i32;
        let bh = (lh * self.scale).ceil() as i32;
        // 窗口在内侧多一圈透明留白：悬停的图标要"一半在 Dock 内、一半在 Dock 外"，
        // 伸出去的半截画在底板之外，没有留白就会被窗口边界裁掉
        let m = (crate::dockview::dock_margin() * self.scale * settings::icon_scale().clamp(0.5, 2.0))
            .ceil() as i32;
        // 留白在内侧：下锚 → 顶部留白（底板贴下）、上锚 → 底部留白、
        // 左锚 → 右侧留白、右锚 → 左侧留白。(bx, by) = 底板在窗口里的原点
        let (w, h, bx, by) = match anchor {
            crate::settings::DockAnchor::Bottom => (bw, bh + m, 0, m),
            crate::settings::DockAnchor::Top => (bw, bh + m, 0, 0),
            crate::settings::DockAnchor::Left => (bw + m, bh, 0, 0),
            crate::settings::DockAnchor::Right => (bw + m, bh, m, 0),
        };
        self.size = (w, h);
        self.board = (bx, by, bw, bh);

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
            .map(|it| {
                if it.dim {
                    return blank.as_slice();
                }
                let k = it.key();
                match self.over_px.iter().find(|(kk, _)| *kk == k) {
                    Some((_, px)) => px.as_slice(),
                    None => it.rgba.as_slice(),
                }
            })
            .collect();
        let out = self.view.paint(
            w, h, self.scale, &icons, self.active, &tray_icons,
            self.active_tray,
            show_tray, show_desktop, self.hovered_desktop, vertical,
            self.mouse,
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

        // 定位：沿 dock_anchor 贴对应的屏幕边（底板对齐屏幕，留白伸向屏幕内侧）。
        // 水平（下/上）按"屏幕矩形"居中；垂直（左/右）竖向居中。
        let mi = crate::window::primary_monitor();
        let bottom = if crate::hide::is_active() { mi.rcMonitor.bottom } else { mi.rcWork.bottom };
        let mon = mi.rcMonitor;
        let (x, y) = match anchor {
            crate::settings::DockAnchor::Bottom => (
                mon.left + (mon.right - mon.left - w) / 2,
                bottom - (cfg::DOCK_BOTTOM_MARGIN as f32 * self.scale) as i32 - h,
            ),
            // 上/左锚：底板原点就是窗口原点，窗口直接贴边
            crate::settings::DockAnchor::Top => (mon.left + (mon.right - mon.left - w) / 2, mon.top),
            crate::settings::DockAnchor::Left => (mon.left, mon.top + (mon.bottom - mon.top - h) / 2),
            // 右锚：底板右缘 = 窗口右缘 - 留白 → 窗口 x = 屏幕右缘 - 窗宽
            crate::settings::DockAnchor::Right => (mon.right - w, mon.top + (mon.bottom - mon.top - h) / 2),
        };
        unsafe {
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
        }

        // 托管模式：把假 Shell_TrayWnd 移动到 Dock 矩形（对齐 C# SetTrayHostSizeData，
        // 让 Shell_NotifyIconGetRect 认 Dock 为新托盘宿主），把每个图标屏幕矩形告诉宿主，
        // 并按真实槽位中心重建 tray_targets（悬停/点击投消息用这个坐标）。
        if self.tray.hosted() {
            // 报底板矩形（不含内侧透明留白），Shell 的托盘命中才落在真实图标区
            crate::trayhost::set_tray_host_size(x + bx, y + by, bw, bh);
            self.sync_host_layout(x, y);
        }

        // 托盘成员/目标变了，闪烁快照跟着刷新（70ms 定时器里也会再发一次）
        self.publish_flash();
    }

    /// 把"正在闪烁的托盘图标"发布成胶囊消费的快照（见 `FlashIcon`）。
    /// 集合或亮/灭帧没变就不动版本号，胶囊那边靠版本号判断要不要重绘。
    fn publish_flash(&mut self) {
        let now = Instant::now();
        // 微信提示音按"闪烁轮次"响：`it.flash_at` 在每次亮→灭时刷新，
        // 拿它当"这一闪"的身份。同一轮连续闪烁（相邻两闪 ~500ms）整轮
        // 只响开头一声；停闪超过 1 秒再闪才算新一轮、再响一声。
        // 按图标 key 各自记，双开微信两个图标互不干扰。
        let mut wx_new_keys: Vec<String> = Vec::new();
        let mut snap: Vec<FlashIcon> = Vec::new();
        let mut sounded = SOUND_FLASH_AT.lock().unwrap_or_else(|e| e.into_inner());
        if settings::show_tray() {
            for &i in &self.tray_shown {
                let Some(it) = self.tray.items.get(i) else { continue };
                let Some(t) = it.flash_at else { continue };
                if now.duration_since(t).as_millis() as u64 > cfg::FLASH_WINDOW_MS {
                    continue;
                }
                let key = it.key();
                if notify::is_wechat(&it.name, &it.exe) {
                    // 微信闪一下 = 新消息：让 UIA 读取线程立刻重读一轮，卡片跟提示音同步。
                    // on_flash 内部按 key + 闪烁时刻去重，同一帧重复调用只唤醒一次。
                    crate::banner::on_flash(&key, t);
                    // 上报「还在闪」：横幅窗口据此决定闪烁横幅什么时候收起
                    crate::banner::flash_seen(&key);
                    let pid = it.host.as_ref().map(|h| h.pid).unwrap_or(0);
                    if settings::wechat_cards() {
                        // UIA 卡片开着：只有读不到会话列表的实例（微信 4.x 多开
                        // 只有第一个实例的树有内容，实测 4.2.0）需要兜底横幅
                        if pid != 0 && !crate::wechatmsg::pid_readable(pid) {
                            crate::banner::on_flash_card(&key, t, pid, &it.name);
                        }
                    } else {
                        // UIA 卡片关着（默认模式）：闪烁直接弹「有新消息」横幅，
                        // 点击跳到对应微信窗口；图标不闪了横幅随之收起
                        crate::banner::on_flash_card(&key, t, pid, &it.name);
                    }
                    // 提示音：只在「开始闪的第一声」响。以 `it.flash_at`（这次亮→灭
                    // 的时刻）为闪烁身份：同一轮连续闪烁里相邻两闪间隔约 500ms
                    // （< FLASH_SOUND_GAP_MS），整轮只响开头一声；停止闪烁 1 秒
                    // 后再次闪烁 → 间隔 ≥ 1 秒，算新一轮，再响一声。
                    // 按图标 key 各记各的，双开微信互不干扰。
                    if settings::wechat_sound() && !settings::wechat_cards() {
                        let new_burst = match sounded.get(&key) {
                            Some(prev) => t
                                .checked_duration_since(*prev)
                                .map(|d| d.as_millis())
                                .unwrap_or(0)
                                >= FLASH_SOUND_GAP_MS,
                            None => true,
                        };
                        sounded.insert(key.clone(), t);
                        if new_burst {
                            wx_new_keys.push(key.clone());
                        }
                    }
                }
                snap.push(FlashIcon {
                    key,
                    name: it.name.clone(),
                    rgba: it.rgba.clone(),
                    dim: it.dim,
                    target: self.host_targets.get(i).cloned().flatten().map(|t| t.raw()),
                });
            }
        }
        for key in wx_new_keys {
            notify::play_wechat(&key);
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
            self.tray_empty_since = None;
            return;
        }
        let hidden = settings::hidden_tray();
        let next: Vec<usize> = (0..n)
            .filter(|&i| {
                self.tray
                    .items
                    .get(i)
                    .map_or(false, |it| !hidden.iter().any(|k| *k == it.key()))
            })
            .collect();
        // 托盘表在应用"删了又加"（重注册图标、微信提示闪一下）的那一瞬间会是空的。
        // 直接把它当成"托盘真的没图标了"会让 Dock 少掉整块托盘区——用户看到的就是
        // 胶囊被截断半截、1 秒后又长回来。宽限期内先沿用上一批槽位，真空了自然会收起。
        if next.is_empty() && !self.tray_shown.is_empty() {
            let since = *self.tray_empty_since.get_or_insert_with(Instant::now);
            if since.elapsed().as_millis() < cfg::TRAY_EMPTY_HOLD_MS {
                crate::vlog!(
                    "[dock] 托盘表瞬时为空，沿用上一批 {} 个图标（不缩尺寸）",
                    self.tray_shown.len()
                );
                return;
            }
        }
        self.tray_empty_since = None;
        self.tray_shown = next;
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

    /// 波浪放大的瞬时值：`(图标被抬出的距离, 放大后的图标边长)`（k=1 逻辑点）。
    /// 与 dockview::paint 同一套公式：光标不在 Dock 上时为 (0, DOCK_ICON)。
    fn pop_wave(&self, axis: f32) -> (f32, f32) {
        let anchor = settings::dock_anchor();
        let vertical = anchor.is_vertical();
        let (gx, gy) = match anchor {
            settings::DockAnchor::Bottom => (0.0, -1.0),
            settings::DockAnchor::Top => (0.0, 1.0),
            settings::DockAnchor::Left => (1.0, 0.0),
            settings::DockAnchor::Right => (-1.0, 0.0),
        };
        let m = crate::dockview::dock_margin();
        let cursor_axis = self.mouse.map(|(x, y)| {
            let x = x - if gx < 0.0 { m } else { 0.0 };
            let y = y - if gy < 0.0 { m } else { 0.0 };
            if vertical { y } else { x }
        });
        let mag = crate::dockview::wave_mag(cursor_axis, axis);
        (
            (cfg::DOCK_H * 0.5) * (mag - 1.0) / cfg::DOCK_WAVE_AMP,
            cfg::DOCK_ICON * mag,
        )
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
                    self.show_tip("返回桌面", cx, cy, ty, 0);
                }
                None => self.hide_tip(),
            }
            return;
        }
        let show_desktop = settings::show_desktop_btn();
        let (text, rect, axis) = if let Some(i) = self.hovered_tray {
            let rect = self.tray_rects.get(i).copied();
            let item = self
                .tray_shown
                .get(i)
                .and_then(|&k| self.tray.items.get(k))
                .map(|it| it.name.clone())
                .unwrap_or_default();
            match rect {
                Some(r) => (
                    item,
                    r,
                    crate::dockview::tray_axis(i, self.items.len(), show_desktop),
                ),
                None => return self.hide_tip(),
            }
        } else if let Some(i) = self.hovered {
            match (self.rects.get(i).copied(), self.items.get(i)) {
                (Some(r), Some(it)) => {
                    (it.tip.clone(), r, crate::dockview::icon_axis(i, show_desktop))
                }
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
        // 弹出避让：悬停图标放大后会越过底板边缘，提示条要贴着**弹出的图标**
        // 外缘摆，不然弹出的半截正好压住提示条（与 dockview::paint 同一套波浪）
        let (rise, side) = self.pop_wave(axis);
        let se = (self.scale * settings::icon_scale().clamp(0.5, 2.0)).max(0.5);
        let (ty, clear) = if matches!(settings::dock_anchor(), settings::DockAnchor::Bottom) {
            // 下锚：提示条在图标上方 —— 直接贴弹出图标的上缘（静止时贴槽位里的图标）
            let dy = (cfg::DOCK_SLOT * 0.5 - rise - side * 0.5) * se;
            (wr.top + rect[1] as i32 + dy.round() as i32, 0)
        } else {
            // 其余锚：图标越过底板边缘的伸出量，提示条整体再挪出去
            let out = ((rise + side * 0.5 - cfg::DOCK_H * 0.5).max(0.0) * se).ceil() as i32;
            (wr.top + rect[1] as i32, out)
        };
        self.show_tip(&text, cx, cy, ty, clear);
    }

    /// 提示条位置随 Dock 锚位走：下 = 图标上方；上 = Dock 下方；
    /// 左 = Dock 右侧；右 = Dock 左侧。文本在锚点方向上居中。
    /// `clear`：弹出图标越过底板边缘的额外让位（物理像素，见 update_tip）。
    fn show_tip(&mut self, text: &str, cx: i32, cy: i32, ty: i32, clear: i32) {
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
        // 底板在屏幕上的边缘（窗口含内侧透明留白，弹层要贴底板而不是贴窗）
        let (bx, by, bw, bh) = self.board;
        let board_l = wr.left + bx;
        let board_t = wr.top + by;
        let board_r = board_l + bw;
        let board_b = board_t + bh;
        let (x, y) = match settings::dock_anchor() {
            crate::settings::DockAnchor::Bottom => (clamp_x(cx - w / 2), ty - h - gap),
            crate::settings::DockAnchor::Top => (clamp_x(cx - w / 2), board_b + gap + clear),
            crate::settings::DockAnchor::Left => (board_r + gap + clear, clamp_y(cy - h / 2)),
            crate::settings::DockAnchor::Right => (board_l - w - gap - clear, clamp_y(cy - h / 2)),
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
        // 静止槽位没点到：再看悬停放大"伸出底板"的那半截
        .or_else(|| self.pop_hit_at(x, y, false))
    }

    fn tray_index_at(&self, x: f32, y: f32) -> Option<usize> {
        self.tray_rects.iter().position(|r| {
            let s = r[2];
            x >= r[0] && x <= r[0] + s && y >= r[1] && y <= r[1] + s
        })
        // 同上：托盘图标放大后冒出底板的部分也算本体的点击/悬停区
        .or_else(|| self.pop_hit_at(x, y, true))
    }

    /// 悬停放大的"弹出矩形"命中：点在某个图标放大后冒出底板的半截上时，
    /// 返回该图标的下标。`tray` = 查托盘图标（false 查应用图标）。
    fn pop_hit_at(&self, x: f32, y: f32, tray: bool) -> Option<usize> {
        let rects = if tray { self.popped_rects().1 } else { self.popped_rects().0 };
        rects.iter().position(|r| {
            x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3]
        })
    }

    /// 各图标悬停放大后的矩形（物理像素 client 坐标）。
    ///
    /// 放大时图标朝屏幕内侧"浮出"，超出底板的部分画在窗口的透明留白里。
    /// 这半截在视觉上是图标本体，命中测试与点击也要算进去——否则用户点
    /// 冒出来的那一半会穿透 Dock 点到桌面。与 `dockview::paint` 同一套波浪公式。
    fn popped_rects(&self) -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
        let (gx, gy) = match settings::dock_anchor() {
            settings::DockAnchor::Bottom => (0.0, -1.0),
            settings::DockAnchor::Top => (0.0, 1.0),
            settings::DockAnchor::Left => (1.0, 0.0),
            settings::DockAnchor::Right => (-1.0, 0.0),
        };
        let se = (self.scale * settings::icon_scale().clamp(0.5, 2.0)).max(0.5);
        let show_desktop = settings::show_desktop_btn();
        let mut app = Vec::with_capacity(self.rects.len());
        for (i, r) in self.rects.iter().enumerate() {
            let (rise, side) = self.pop_wave(crate::dockview::icon_axis(i, show_desktop));
            app.push(popped_rect(r, gx, gy, rise, side, se));
        }
        let mut tray = Vec::with_capacity(self.tray_rects.len());
        for (j, r) in self.tray_rects.iter().enumerate() {
            let (rise, side) =
                self.pop_wave(crate::dockview::tray_axis(j, self.items.len(), show_desktop));
            tray.push(popped_rect(r, gx, gy, rise, side, se));
        }
        (app, tray)
    }

    /// 是否点在右端"返回桌面"细长条上（命中区比细条宽一圈）
    fn desktop_at(&self, x: f32, y: f32) -> bool {
        self.desktop_rect
            .map_or(false, |r| x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3])
    }

    /// 显示桌面 / 还原（等价 Win+D）。
    ///
    /// 之前走 `IShellDispatch4::ToggleDesktop`，实测部分环境下（原生任务栏被
    /// 接管隐藏后）点了没反应；改成直接发一次 Win+D 系统组合键，行为与用户
    /// 亲手按键盘完全一致。桌面天气卡片自己负责在被收起后恢复（见 weathercard.rs）。
    fn toggle_desktop(&self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
            VK_LWIN,
        };
        const VK_D: u16 = 0x44; // 'D'
        let key = |vk: u16, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            let inputs = [
                key(VK_LWIN.0, false),
                key(VK_D, false),
                key(VK_D, true),
                key(VK_LWIN.0, true),
            ];
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
        crate::vlog!("[dock] 返回桌面（Win+D）");
    }

    /// 重建"用户指定了图标"的托盘项像素缓存（设置版本或图标尺寸变了才做）
    fn refresh_icon_over(&mut self, px: i32) {
        let ver = settings::version();
        if self.over_key == (ver, px) {
            return;
        }
        self.over_key = (ver, px);
        self.over_px.clear();
        let keys: Vec<String> = self.tray.items.iter().map(|it| it.key()).collect();
        for k in keys {
            if settings::icon_override(&k).is_none() {
                continue;
            }
            if let Some(rgba) = override_icon(&k, px) {
                self.over_px.push((k, rgba));
            }
        }
    }

    /// 查前台落在哪个 Dock 图标、哪个托盘图标上：返回 (应用下标, 托盘下标)。
    ///
    /// 应用位按"该程序的任一窗口是不是前台"判定（多开折叠后不能只比一个句柄），
    /// 拿不到再退回 exe 比对；托盘位按托盘图标所属 exe 比对（explorer 那批没 exe 的不参与）。
    ///
    /// 前台窗口先过一遍 [`foreground_window`] 的筛子：点桌面 / 任务栏之后
    /// `GetForegroundWindow` 给的是 `Progman`/`WorkerW`/`Shell_TrayWnd`，属主同样是
    /// explorer.exe，不过滤就会把指示条一直挂在"文件资源管理器"图标下面。
    fn foreground_hits(&self) -> (Option<usize>, Option<usize>) {
        let Some(fg) = foreground_window() else {
            return (None, None);
        };
        let exe = exe_path(fg).map(|s| s.to_lowercase());
        // 应用位按 `window_key`（UWP 是 AUMID，其余是 exe）比对，托盘位仍比真 exe
        let key = window_key(fg);
        let app = self
            .items
            .iter()
            .position(|it| it.wins.iter().any(|h| *h == fg))
            .or_else(|| {
                key.as_ref().and_then(|k| {
                    self.items
                        .iter()
                        .position(|it| !it.exe.is_empty() && it.exe == *k)
                })
            });
        let tray = exe.as_ref().and_then(|e| {
            self.tray_shown.iter().position(|&k| {
                self.tray
                    .items
                    .get(k)
                    .map(|it| !it.exe.is_empty() && it.exe.eq_ignore_ascii_case(e))
                    .unwrap_or(false)
            })
        });
        (app, tray)
    }

    /// 重算前台指示条；返回是否有变化（变了才重绘）
    fn refresh_active(&mut self) -> bool {
        let (app, tray) = self.foreground_hits();
        let changed = app != self.active || tray != self.active_tray;
        self.active = app;
        self.active_tray = tray;
        changed
    }

    fn activate(&mut self, idx: usize) {
        let Some(item) = self.items.get(idx) else { return };
        // 程序已经在跑：切到已开的窗口，别再启动一个（钉选项也一样）
        match item.wins.len() {
            1 => {
                if let Some(h) = item.wins.first() {
                    activate_hwnd(*h);
                }
                return;
            }
            n if n > 1 => {
                self.open_winlist(idx);
                return;
            }
            _ => {}
        }
        if let Target::Lnk(path) = &item.target {
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
    }

    // ---------- 同程序多开：窗口选择弹窗 ----------

    /// 在图标旁边弹出窗口列表（多开时点一下图标走这里）
    fn open_winlist(&mut self, idx: usize) {
        let Some(item) = self.items.get(idx) else { return };
        let rows: Vec<(HWND, String)> = item
            .wins
            .iter()
            .map(|h| (*h, window_title(*h).unwrap_or_else(|| item.name.clone())))
            .collect();
        if rows.is_empty() {
            return;
        }
        let titles: Vec<String> = rows.iter().map(|r| r.1.clone()).collect();
        let (w, h) = self.view.winlist_size(&titles, self.scale);
        if w <= 0 || h <= 0 {
            return;
        }
        self.winlist_rows = rows;
        self.winlist_hover = None;
        self.winlist_left = None;
        self.winlist_size = (w, h);
        self.paint_winlist();
        let Some(rect) = self.rects.get(idx).copied() else { return };
        let (x, y) = self.place_popup(rect, w, h);
        unsafe {
            let _ = SetWindowPos(self.winlist, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
            let _ = ShowWindow(self.winlist, SW_SHOWNOACTIVATE);
        }
        crate::vlog!("[dock] 窗口列表 {} 行 {w}x{h} @({x},{y})", self.winlist_rows.len());
    }

    fn paint_winlist(&mut self) {
        if self.winlist.is_invalid() || self.winlist_rows.is_empty() {
            return;
        }
        let titles: Vec<String> = self.winlist_rows.iter().map(|r| r.1.clone()).collect();
        let (w, h) = self.winlist_size;
        let (rgba, rects) = self
            .view
            .paint_winlist(&titles, self.winlist_hover, self.scale, w, h);
        if rgba.is_empty() {
            return;
        }
        self.winlist_rects = rects;
        let bgra = render::premultiplied_rgba_to_bgra(&rgba);
        render::present(self.winlist, &bgra, w, h);
    }

    /// 弹窗摆放：跟着 Dock 锚位走（下=图标上方，上=下方，左=右侧，右=左侧），
    /// 主轴方向按图标中心居中，最后夹在显示器工作区内。
    fn place_popup(&self, rect: [f32; 3], w: i32, h: i32) -> (i32, i32) {
        let mut wr = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut wr);
        }
        let cx = wr.left + (rect[0] + rect[2] / 2.0) as i32;
        let cy = wr.top + (rect[1] + rect[2] / 2.0) as i32;
        let top = wr.top + rect[1] as i32;
        let bottom = top + rect[2] as i32;
        let gap = (cfg::WINLIST_GAP as f32 * self.scale) as i32;
        let mon = crate::window::primary_monitor().rcMonitor;
        let clamp_x = |x: i32| x.clamp(mon.left + 4, (mon.right - w - 4).max(mon.left + 4));
        let clamp_y = |y: i32| y.clamp(mon.top + 4, (mon.bottom - h - 4).max(mon.top + 4));
        // 底板在屏幕上的边缘（窗口含内侧透明留白）；上下锚位用不到底板高/顶
        let (bx, _by, bw, _bh) = self.board;
        let board_l = wr.left + bx;
        let board_r = board_l + bw;
        match settings::dock_anchor() {
            crate::settings::DockAnchor::Bottom => (clamp_x(cx - w / 2), clamp_y(top - h - gap)),
            crate::settings::DockAnchor::Top => (clamp_x(cx - w / 2), clamp_y(bottom + gap)),
            crate::settings::DockAnchor::Left => (clamp_x(board_r + gap), clamp_y(cy - h / 2)),
            crate::settings::DockAnchor::Right => (clamp_x(board_l - w - gap), clamp_y(cy - h / 2)),
        }
    }

    fn winlist_is_open(&self) -> bool {
        !self.winlist.is_invalid() && unsafe { IsWindowVisible(self.winlist).as_bool() }
    }

    fn hide_winlist(&mut self) {
        if !self.winlist.is_invalid() {
            unsafe {
                let _ = ShowWindow(self.winlist, SW_HIDE);
            }
        }
        self.winlist_rows.clear();
        self.winlist_rects.clear();
        self.winlist_hover = None;
        self.winlist_left = None;
    }

    /// 弹窗自动收起：鼠标离开"弹窗 + Dock"整片区域超过 `WINLIST_GRACE_MS` 才隐藏。
    /// 从 70ms 的 `T_FLASH` 里调用，不额外开定时器。
    ///
    /// 为什么把 Dock 也算"里面"：弹窗是从 Dock 图标点出来的，刚弹出时光标必然还在
    /// 图标上；只按弹窗矩形判定的话，用户还没移过去就被收回去了（450ms 根本不够）。
    fn winlist_tick(&mut self) -> bool {
        if !self.winlist_is_open() {
            return false;
        }
        let inside = unsafe {
            let mut pt = POINT::default();
            if GetCursorPos(&mut pt).is_err() {
                false
            } else {
                let hit = |hwnd: HWND| {
                    if hwnd.is_invalid() {
                        return false;
                    }
                    let mut wr = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut wr);
                    pt.x >= wr.left && pt.x < wr.right && pt.y >= wr.top && pt.y < wr.bottom
                };
                hit(self.winlist) || hit(self.hwnd)
            }
        };
        if inside {
            self.winlist_left = None;
            return false;
        }
        match self.winlist_left {
            None => {
                // 指针一离开就先把高亮行清掉，别让最后一行亮着等 450ms
                if self.winlist_hover.take().is_some() {
                    self.paint_winlist();
                }
                self.winlist_left = Some(Instant::now());
                false
            }
            Some(t) => {
                if t.elapsed().as_millis() >= cfg::WINLIST_GRACE_MS {
                    self.hide_winlist();
                    true
                } else {
                    false
                }
            }
        }
    }

    /// 弹窗里点了第 i 行：切到那个窗口
    fn winlist_pick(&mut self, i: usize) {
        if let Some((h, _)) = self.winlist_rows.get(i).map(|r| (r.0, r.1.clone())) {
            activate_hwnd(h);
        }
        self.hide_winlist();
    }

    fn winlist_hit(&self, x: f32, y: f32) -> Option<usize> {
        self.winlist_rects.iter().position(|r|
            x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3])
    }
}

/// 提示条窗口：分层 + 置顶 + 点击穿透，不抢焦点也不会挡鼠标
/// 窗口选择弹窗：分层 + 置顶 + 工具窗，**不点击穿透**（要收鼠标）、
/// 但带 `WS_EX_NOACTIVATE`——点它不抢焦点，选完直接把目标窗口拉前台。
unsafe fn create_winlist_window(dock: *mut Dock) -> HWND {
    let hinstance = GetModuleHandleW(None).unwrap_or_default();
    // 重复注册返回 0，忽略即可（窗口类引用计数不会故意泄漏）
    let wc = WNDCLASSW {
        lpfnWndProc: Some(winlist_proc),
        hInstance: hinstance.into(),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        lpszClassName: w!("ORBIT_WINLIST"),
        ..Default::default()
    };
    RegisterClassW(&wc);
    CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        w!("ORBIT_WINLIST"),
        PCWSTR::null(),
        WS_POPUP,
        0,
        0,
        10,
        10,
        HWND::default(),
        HMENU::default(),
        hinstance,
        Some(dock as *const core::ffi::c_void),
    )
    .unwrap_or_default()
}

/// 弹窗自己的窗口过程：悬停高亮 + 左键选窗口 + 右键/中键收起
pub unsafe extern "system" fn winlist_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CREATE {
            let cs = *(l.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return LRESULT(0);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dock;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let dock = &mut *ptr;
        let lx = || (l.0 & 0xFFFF) as u16 as i16 as i32 as f32;
        let ly = || ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32 as f32;
        match msg {
            WM_MOUSEMOVE => {
                let h = dock.winlist_hit(lx(), ly());
                dock.winlist_left = None;
                if h != dock.winlist_hover {
                    dock.winlist_hover = h;
                    dock.paint_winlist();
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                if let Some(i) = dock.winlist_hit(lx(), ly()) {
                    dock.winlist_pick(i);
                }
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                dock.hide_winlist();
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
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

/// 查"图标指定"：给某个托盘图标键返回用户指定的图标像素。
///
/// 托盘图标常只有 16px 的 hIcon，放大到 Dock 尺寸必然糊。用户在设置页指定一个
/// 图标源（exe / dll / .ico / png…）后，这里读它的图标顶上；没指定 / 读不到返回 None。
/// 只作用于托盘图标：钉选项和运行项读的是 exe 自带大图标，不参与"指定"。
pub fn override_icon(key: &str, px: i32) -> Option<Vec<u8>> {
    let path = settings::icon_override(key)?;
    let p = PathBuf::from(path.replace('/', "\\"));
    let rgba = large_icon_of_path(&p, px);
    if rgba.is_empty() {
        crate::tlog!("[dock] 指定的图标源 {path} 读不到图标");
        return None;
    }
    Some(rgba)
}

/// 当前生效的固定图标（exe / .lnk 全路径，顺序即显示顺序）。
///
/// 设置页还没改过（`settings::dock_pins()` = None）时回落到系统任务栏的钉选，
/// 这样从旧版本升上来的观感不变；一旦在设置页增删过，就以 Orbit 自己的列表为准。
pub fn effective_pins() -> Vec<String> {
    match settings::dock_pins() {
        Some(list) => list,
        None => pinned_links()
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
    }
}

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
/// 用户进程的所有可见窗口按 exe 分组（前台窗口排第一，其余按枚举顺序）。
/// 同一程序开多个窗口时 Dock 上只留一个图标，左键再弹窗挑窗口。
fn running_groups() -> BTreeMap<String, Vec<HWND>> {
    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut hwnds as *mut Vec<HWND> as isize));
    }
    let fg = foreground_window();
    let mut map: BTreeMap<String, Vec<HWND>> = BTreeMap::new();
    for hwnd in hwnds {
        // 键 = UWP 的 AUMID 或 exe 全路径：同一个 ApplicationFrameHost 里的不同 UWP
        // 应用必须分开，否则「设置」「照片」会挤在最前面那一项上
        let Some(key) = window_key(hwnd) else { continue };
        map.entry(key).or_default().push(hwnd);
    }
    // 前台窗口提到组首：布局别按枚举顺序乱动，但"点一下切过去"要命中用户正在看的那个
    if let Some(fg) = fg {
        for wins in map.values_mut() {
            if let Some(pos) = wins.iter().position(|h| *h == fg) {
                wins.swap(0, pos);
            }
        }
    }
    map
}

fn running_apps() -> BTreeMap<String, HWND> {
    running_groups()
        .into_iter()
        .filter_map(|(key, wins)| wins.first().map(|h| (key, *h)))
        .collect()
}

/// 设置页「添加固定图标」用：正在运行的应用（显示名, exe 全路径），按名字排序。
/// UWP 项（键是 `uwp:<AUMID>`）不列：固定项要落成 .lnk/exe 路径，AUMID 固定不了。
pub fn running_app_list() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = running_apps()
        .into_keys()
        .filter(|k| !k.starts_with(UWP_PREFIX))
        .map(|exe| {
            let name = PathBuf::from(&exe)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| exe.clone());
            (name, exe)
        })
        .collect();
    out.sort_by_key(|(n, _)| n.to_lowercase());
    out
}

/// 切到一个窗口：还原（最小化过）+ 把它拉到最前。
/// Dock 图标点击、窗口列表选择共用这一条。
/// 老实现用 AttachThreadInput 抢前台——会把目标进程的输入队列挂到本线程上，
/// detach 失败时目标就再也收不到鼠标/键盘（见 `window::activate_window`）。
fn activate_hwnd(hwnd: HWND) {
    crate::window::activate_window(hwnd);
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

/// 这些类名的窗口属主是 explorer.exe（桌面 / 任务栏 / 开始菜单），或者是本程序
/// 自己的分层窗。它们出现在"前台"不代表用户切到了某个程序：桌面和任务栏都不该
/// 点亮 Dock 指示条，也不该出现在运行中应用列表里。
fn is_shell_class(cls: &str) -> bool {
    matches!(
        cls,
        "Progman"
            | "WorkerW"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "NotifyIconOverflowWindow"
            | "TopLevelWindowForOverflowXamlIsland"
            | "Windows.UI.Core.CoreWindow"
            | "XamlExplorerHostIslandWindow"
            | "MultitaskingViewFrame"
            | "ForegroundStaging"
            | "TaskListThumbnailWnd"
            | "ORBIT_CAPSULE"
            | "ORBIT_DOCK"
    )
}

/// 窗口类名（取不到返回空串）
fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// 真正"用户切过去的程序窗口"：无效 / 不可见 / cloaked / 外壳窗口一律返回 None。
///
/// 桌面上点一下、任务栏上点一下、Win+D 之后，前台会落到 Progman / WorkerW /
/// Shell_TrayWnd 上；它们的 exe 也是 explorer.exe，直接拿 exe 比 Dock 图标就会
/// 误判成"文件资源管理器在前台"。关掉的 UWP 还会留下 cloaked 的前台窗，同样跳过。
fn foreground_window() -> Option<HWND> {
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() || !unsafe { IsWindowVisible(fg).as_bool() } {
        return None;
    }
    let mut cloaked = 0u32;
    if unsafe {
        DwmGetWindowAttribute(
            fg,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
    }
    .is_ok()
        && cloaked != 0
    {
        return None;
    }
    if is_shell_class(&window_class(fg)) {
        return None;
    }
    Some(fg)
}

// ---------- 打包应用（UWP）：顶层窗是宿主进程的壳，真正的 App 靠 AUMID 认 ----------

/// UWP 项在 `DockItem.exe` 里存 `uwp:<AUMID>` 当分组键（普通应用存 exe 全路径）。
const UWP_PREFIX: &str = "uwp:";

/// `PKEY_AppUserModel_ID`（`System.AppUserModel.ID`）。windows crate 把它挂在
/// `Win32_Storage_EnhancedStorage` 上，为一个常量不值得多开一个 feature，直接按 GUID + pid 写死。
const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
    pid: 5,
};

/// 窗口上挂的 AppUserModelID。UWP 的 `ApplicationFrameWindow` 上挂的是**宿主 App** 的
/// AUMID（`Microsoft.Windows.Photos_8wekyb3d8bbwe!App` 这种），不是 ApplicationFrameHost。
fn window_aumid(hwnd: HWND) -> Option<String> {
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let pv = store.GetValue(&PKEY_APP_USER_MODEL_ID).ok()?;
        let s = pv.to_string().trim().to_string();
        (!s.is_empty()).then_some(s)
    }
}

/// Dock 的分组键：UWP 用 `uwp:<AUMID>`。
///
/// 所有 UWP 应用的顶层窗都是同一个 ApplicationFrameHost.exe 进程里的
/// `ApplicationFrameWindow`，按 exe 分组会把「设置」「照片」全并成一个
/// ApplicationFrameHost 项——而这个 exe 根本没有图标资源，所以那个项永远没有图标。
/// 其余应用仍按 exe 全路径（小写）分组。
fn window_key(hwnd: HWND) -> Option<String> {
    if window_class(hwnd).eq_ignore_ascii_case("ApplicationFrameWindow") {
        if let Some(a) = window_aumid(hwnd) {
            return Some(format!("{UWP_PREFIX}{a}"));
        }
    }
    exe_path(hwnd).map(|s| s.to_lowercase())
}

/// 按 AUMID 取应用图标（开始菜单/任务栏显示的那一张）：走 `shell:AppsFolder\<AUMID>`
/// 的 `IShellItemImageFactory`，能拿到 App 的 256px logo，比宿主 exe 的通用图标清楚得多。
///
/// 注意 `SHCreateItemFromParsingName` 不认 `shell:` 协议（实测 0x80070003 找不到路径），
/// 必须先 `SHParseDisplayName` 把外壳命名空间解析成 PIDL 再建 item。
fn aumid_icon(aumid: &str, px: i32) -> Vec<u8> {
    if px <= 0 {
        return Vec::new();
    }
    let path = format!("shell:AppsFolder\\{aumid}");
    let wide = to_wide(std::ffi::OsStr::new(&path));
    unsafe {
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        if let Err(e) = SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None) {
            crate::vlog!("[dock] 解析 {aumid} 失败: {e:?}");
            return Vec::new();
        }
        if pidl.is_null() {
            crate::vlog!("[dock] {aumid} 解析出空 PIDL");
            return Vec::new();
        }
        let item: windows::core::Result<IShellItemImageFactory> = SHCreateItemFromIDList(pidl);
        ILFree(Some(pidl));
        let item = match item {
            Ok(i) => i,
            Err(e) => {
                crate::vlog!("[dock] AppsFolder item 打不开 {aumid}: {e:?}");
                return Vec::new();
            }
        };
        let size = SIZE { cx: px, cy: px };
        match item.GetImage(size, SIIGBF(SIIGBF_ICONONLY.0 | SIIGBF_BIGGERSIZEOK.0)) {
            Ok(hbm) => render::bitmap_rgba(hbm, px).unwrap_or_default(),
            Err(e) => {
                crate::vlog!("[dock] 取 {aumid} 的图标失败: {e:?}");
                Vec::new()
            }
        }
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
        if is_shell_class(&cls) {
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

/// 取 256px 系统图像列表图标并高质量缩放到 px；取不到则退回 32px 直接绘制。
/// `.ico` 走 `LoadImageW`（外壳对 .ico 只回"图标文件"的类型图标，不是里面那张图），
/// 常见位图（png/jpg…）走 GDI+ 解码 —— 这样「图标指定」既能挑程序也能挑图片。
pub(crate) fn large_icon_of_path(path: &std::path::Path, px: i32) -> Vec<u8> {
    if path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("ico"))
        .unwrap_or(false)
    {
        if let Some(rgba) = icon_from_ico_file(path, px) {
            return rgba;
        }
    }
    if is_bitmap_ext(path) {
        if let Some(rgba) = icon_from_image_file(path, px) {
            return rgba;
        }
    }
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

/// 从 `.ico` 文件读图标。图标文件里一般并存 16/32/48/256 多档，
/// 先按目标边长取（命中即像素级清晰），退而取 256 再高质量缩放。
fn icon_from_ico_file(path: &std::path::Path, px: i32) -> Option<Vec<u8>> {
    let wide = to_wide(path.as_os_str());
    unsafe {
        for size in [px, 256, 48] {
            let h = LoadImageW(
                None,
                PCWSTR(wide.as_ptr()),
                IMAGE_ICON,
                size,
                size,
                LR_LOADFROMFILE,
            );
            let Ok(h) = h else { continue };
            let hicon = HICON(h.0);
            let rgba = render::capture_icon_rgba(hicon, size);
            let _ = DestroyIcon(hicon);
            let Some(rgba) = rgba else { continue };
            if size == px {
                return Some(rgba);
            }
            return Some(render::resize_rgba(&rgba, size, size, px, px));
        }
        None
    }
}

/// 位图图标源（GDI+ 能解的格式；.ico 另走 `LoadImageW`）
fn is_bitmap_ext(path: &std::path::Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) => matches!(
            e.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "bmp" | "gif"
        ),
        None => false,
    }
}

/// 从图片文件读图标：GDI+ 解码 + 居中裁方 + 缩放到 px
fn icon_from_image_file(path: &std::path::Path, px: i32) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    render::decode_cover(&bytes, px)
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
/// 由静止槽位 `[x, y, size]` 算悬停放大后的图标矩形：
/// 中心沿 `(gx, gy)` 浮出 `rise`，边长放大到 `side`；`se` 为逻辑点 → 物理像素系数。
/// 与 dockview::paint 画图标的同一套几何（中心 = 槽位中心 + 法向 rise，边长 = isz*mag）。
fn popped_rect(r: &[f32; 3], gx: f32, gy: f32, rise: f32, side: f32, se: f32) -> [f32; 4] {
    let s = r[2];
    let cx = r[0] + s * 0.5 + gx * rise * se;
    let cy = r[1] + s * 0.5 + gy * rise * se;
    let half = side * se * 0.5;
    [cx - half, cy - half, side * se, side * se]
}

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
