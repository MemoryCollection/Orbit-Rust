//! 开始菜单（接管系统 Win 键）：按 Win 弹出，再按一次收起；点菜单外也收起。
//!
//! 布局（上下两栏）：
//! - 上栏：应用快捷方式网格（一行 4 个，最多 8 个），列表在设置页「开始菜单」
//!   里增删，也可以直接在菜单里按住图标拖到别的格子换位（松手写回设置）。
//!   图标从 exe / .lnk 里取 64px 源图。
//! - 下栏：三个按钮 —— 关机（弹 关机/重启/锁屏 小菜单）、设置（打开 Orbit 设置）、
//!   系统（打开 Windows 系统设置 ms-settings:）。
//!
//! 窗口形态与 banner.rs 同款：置顶分层小窗 + NOACTIVATE + 圆角命中，
//! egui 软件光栅化（egui_soft）。菜单开着时挂 WH_MOUSE_LL 鼠标钩子监听
//! "点在菜单外"（不吞消息，只顺带收起自己）。
//!
//! Win 键接管由低级键盘钩子（WH_KEYBOARD_LL）实现，见下方 key_proc：
//! 裸按 Win（按住期间没碰过任何别的键/鼠标）→ 放行真实抬起（修饰位正常
//! 复位）+ 紧跟一个未分配哑键（vk 0xE8）让系统认为 Win 参与了组合 →
//! 系统开始菜单不弹，本菜单切换；按住期间按过别的键（Win+E、Win+点击）
//! → 原样放行，组合键不受影响。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use eframe::egui;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, GetWindowLongPtrW,
    GetWindowRect, PostMessageW, RegisterClassW, SetWindowsHookExW, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, UnhookWindowsHookEx, CREATESTRUCTW, GWLP_USERDATA, HMENU, HHOOK,
    HWND_TOPMOST, HTCLIENT, HTTRANSPARENT, IDC_ARROW, KBDLLHOOKSTRUCT, LoadCursorW, MSLLHOOKSTRUCT,
    SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_CREATE,
    WM_DESTROY, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEMOVE,
    WM_NCHITTEST, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::config as cfg;
use crate::egui_soft::{argb, ellipsize, glass_board, with_alpha, SoftBackend};
use crate::guard::PowerAction;
use crate::render;

/// 打开菜单（窗口线程投递）
const WM_START_OPEN: u32 = WM_APP + 0x61;
/// 收起菜单（窗口线程投递；鼠标钩子里点菜单外也走它）
const WM_START_CLOSE: u32 = WM_APP + 0x62;
/// 切换菜单（键盘钩子吃掉裸 Win 抬起后投递）
const WM_START_TOGGLE: u32 = WM_APP + 0x63;
/// 图标拖动结束（低级鼠标钩子在任意位置收到左键抬起时投递，避免拖出窗口后收不到）
const WM_START_DROP: u32 = WM_APP + 0x64;
/// windows 导入里没带这几个常用消息/修饰位
const WM_LBUTTONUP: u32 = 0x0202;
const MK_LBUTTON: usize = 0x1;
/// 拖动越过后置位：钩子据此在所有位置收尾
static DRAG_ACTIVE: AtomicBool = AtomicBool::new(false);

static START_HWND: AtomicIsize = AtomicIsize::new(0);
static OPEN: AtomicBool = AtomicBool::new(false);
/// 常驻低级鼠标钩子句柄（点菜单外收起 + 标记 Win 组合）
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);
/// 常驻低级键盘钩子句柄（接管 Win 键，见 key_proc）
static KEY_HOOK: AtomicIsize = AtomicIsize::new(0);
/// 当前按住的 Win 键（VK_LWIN / VK_RWIN；0 = 没按住）
static WIN_DOWN: AtomicU32 = AtomicU32::new(0);
/// Win 按住期间是否碰过别的键（键盘/鼠标）——碰过就算组合键，松 Win 时放行
static WIN_COMBO: AtomicBool = AtomicBool::new(false);
/// 「关闭→最小化」吞掉了按下事件，抬起事件也要一并吞掉
static SWALLOW_UP: AtomicBool = AtomicBool::new(false);

const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

// ---------- 逻辑尺寸（96 DPI 基准） ----------

/// 菜单总宽
const MENU_W: f32 = 420.0;
/// 四周内边距
const PAD: f32 = 16.0;
/// 快捷方式格子边长（图标 + 文字）
const TILE: f32 = 88.0;
/// 格子间距
const TILE_GAP: f32 = 12.0;
/// 最多显示几条快捷方式（每行数量 × 2 行，上限定义在 settings 里）
fn max_tiles() -> usize {
    crate::settings::start_max_pins()
}
/// 上栏与下栏之间的间隔（含分隔线）
const GRID_BAR_GAP: f32 = 12.0;
/// 下栏按钮条高度
const BAR_H: f32 = 58.0;
/// 关机小菜单的宽 / 行高
const POWER_W: f32 = 150.0;
const POWER_ROW_H: f32 = 38.0;
const POWER_PAD: f32 = 8.0;
/// 圆角
const RADIUS: f32 = 14.0;

struct Menu {
    soft: SoftBackend,
    /// 当前快捷方式（settings::start_pins，截到 MAX_TILES）
    pins: Vec<(String, String)>,
    /// 目标路径 → 图标 RGBA（straight，64px 源图）
    icons: HashMap<String, Vec<u8>>,
    /// 上栏格子命中矩形（逻辑点）
    tiles: Vec<[f32; 4]>,
    /// 下栏按钮命中矩形（关机/设置/系统，逻辑点）
    buttons: Vec<[f32; 4]>,
    /// 关机小菜单的行矩形（关着时为空）
    power_rows: Vec<[f32; 4]>,
    /// 关机小菜单卡片的位置（逻辑点）
    power_x: f32,
    power_y: f32,
    power_open: bool,
    hover_tile: Option<usize>,
    hover_btn: Option<usize>,
    hover_power: Option<usize>,
    /// 图标拖动：按下时记下的格子与起点（逻辑点）；移动越过阈值才算拖动
    press_tile: Option<usize>,
    press_pos: (f32, f32),
    dragging: bool,
    /// 拖动时悬停的目标格子
    drag_target: Option<usize>,
    /// 菜单逻辑高度
    menu_h: f32,
    /// 物理像素窗口尺寸
    size: (i32, i32),
    mouse_tracked: bool,
}

pub fn init(instance: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("ORBIT_START"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let p = Box::into_raw(Box::new(Menu {
            soft: SoftBackend::new(),
            pins: Vec::new(),
            icons: HashMap::new(),
            tiles: Vec::new(),
            buttons: Vec::new(),
            power_rows: Vec::new(),
            power_x: 0.0,
            power_y: 0.0,
            power_open: false,
            hover_tile: None,
            hover_btn: None,
            hover_power: None,
            press_tile: None,
            press_pos: (0.0, 0.0),
            dragging: false,
            drag_target: None,
            menu_h: 0.0,
            size: (0, 0),
            mouse_tracked: false,
        }));
        if let Ok(h) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("ORBIT_START"),
            PCWSTR::null(),
            WS_POPUP,
            0, 0, 10, 10,
            HWND::default(), HMENU::default(), instance,
            Some(p as *const core::ffi::c_void),
        ) {
            START_HWND.store(h.0 as isize, Ordering::Relaxed);
        } else {
            drop(Box::from_raw(p));
        }
        // 常驻钩子（键盘接管 Win 键 + 鼠标标组合/点外收起）。挂在 init 所在的
        // 主线程上——它有消息泵，低级钩子才回得来。进程退出时随进程销毁。
        let hmod = GetModuleHandleW(None).unwrap_or_default();
        if let Ok(hk) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), HINSTANCE(hmod.0), 0) {
            KEY_HOOK.store(hk.0 as isize, Ordering::Relaxed);
        } else {
            crate::tlog!("[start] 键盘钩子安装失败，Win 键接管不可用");
        }
        if let Ok(hk) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), HINSTANCE(hmod.0), 0) {
            MOUSE_HOOK.store(hk.0 as isize, Ordering::Relaxed);
        }
    }
}

fn hwnd() -> HWND {
    HWND(START_HWND.load(Ordering::Relaxed) as *mut _)
}

pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// 开 / 收（Win 热键走这里；主线程调用）
pub fn toggle() {
    if is_open() {
        hide();
    } else {
        show();
    }
}

pub fn show() {
    let h = hwnd();
    if h.is_invalid() || OPEN.swap(true, Ordering::Relaxed) {
        return;
    }
    unsafe {
        let _ = PostMessageW(h, WM_START_OPEN, WPARAM(0), LPARAM(0));
    }
}

pub fn hide() {
    let h = hwnd();
    if h.is_invalid() {
        return;
    }
    unsafe {
        let _ = PostMessageW(h, WM_START_CLOSE, WPARAM(0), LPARAM(0));
    }
}

/// 低级鼠标钩子，两件事：
/// 1. 菜单开着时左/右键点在菜单外 → 投递收起（不吞消息，别的窗口照常收到点击）；
/// 2. Win 按住期间点了鼠标（Win+点击）→ 标记组合键，松 Win 时放行不弹菜单。
unsafe extern "system" fn mouse_proc(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = w.0 as u32;
        if WIN_DOWN.load(Ordering::Relaxed) != 0
            && matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN)
        {
            WIN_COMBO.store(true, Ordering::Relaxed);
        }
        // 微信「关闭」按钮 → 最小化（不让它隐藏到托盘，否则 UIA 树整棵被摘掉）
        if msg == WM_LBUTTONDOWN {
            let ms = l.0 as *const MSLLHOOKSTRUCT;
            if !ms.is_null() && (*ms).flags & 0x1 == 0 {
                let pt = (*ms).pt;
                if crate::wechatmsg::hit_close_and_minimize(pt.x, pt.y) {
                    SWALLOW_UP.store(true, Ordering::Relaxed);
                    return LRESULT(1);
                }
            }
        }
        if msg == WM_LBUTTONUP && SWALLOW_UP.swap(false, Ordering::Relaxed) {
            // 按下已被吞掉，抬起也吞掉，免得微信收到一个孤立的抬键
            return LRESULT(1);
        }
        // 拖动图标时，指针可能已经移出菜单窗口：钩子在所有位置收左键抬起，
        // 换算成窗口内坐标投递给窗口线程收尾（窗口自己的 WM_LBUTTONUP 也会走同一入口，幂等）
        if msg == WM_LBUTTONUP && DRAG_ACTIVE.load(Ordering::Relaxed) {
            let ms = l.0 as *const MSLLHOOKSTRUCT;
            if !ms.is_null() {
                let pt = (*ms).pt;
                let mut r = RECT::default();
                if GetWindowRect(hwnd(), &mut r).is_ok() {
                    let x = (pt.x - r.left).max(0) as u16 as isize;
                    let y = (pt.y - r.top).max(0) as u16 as isize;
                    let _ = PostMessageW(hwnd(), WM_START_DROP, WPARAM(0), LPARAM((y << 16) | x));
                }
            }
        }
        if OPEN.load(Ordering::Relaxed)
            && matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN)
        {
            let ms = l.0 as *const MSLLHOOKSTRUCT;
            if !ms.is_null() {
                let pt = (*ms).pt;
                let mut r = RECT::default();
                let _ = GetWindowRect(hwnd(), &mut r);
                let inside =
                    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom;
                if !inside {
                    let _ = PostMessageW(hwnd(), WM_START_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
    CallNextHookEx(None, code, w, l)
}

/// 低级键盘钩子：接管 Win 键。
/// - 按下 LWIN/RWIN：记住按住的是哪颗、清组合标记，**放行**（Win+E 的 E
///   按下来才带着 Win 修饰位）；
/// - Win 按住期间按下任何别的键：标记组合键；
/// - 松开 Win：**放行**真实的抬起事件（修饰位正常清除，绝不吞——吞了系统
///   就以为 Win 还按着，之后的按键全被当成 Win+组合，键位全乱）。裸按
///   （没碰过别的键/鼠标）时紧跟着发送一个未分配键（vk 0xE8）的按下+抬起：
///   系统认为 Win 参与了组合，shell 就不会在这颗键的抬起上弹开始菜单
///   （AutoHotkey 社区 `~LWin up:: Send {Blind}{vkE8}` 的同款方案），
///   同时切换本菜单。
/// - 其它键、以及一切注入事件（LLKHF_INJECTED，比如我们自己的哑键、
///   其他工具的 SendInput）一律放行，永不吞。
unsafe extern "system" fn key_proc(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::KBDLLHOOKSTRUCT_FLAGS;
    const LLKHF_INJECTED: u32 = 0x10;
    if code >= 0 {
        let kb = l.0 as *const KBDLLHOOKSTRUCT;
        if !kb.is_null() {
            let flags = (*kb).flags;
            let vk = (*kb).vkCode;
            let msg = w.0 as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            // 注入事件（哑键/其他工具的模拟输入）不是用户动作，全部放行，
            // 也不参与组合判定——否则会自我递归或把别的软件的模拟输入当真
            if flags & KBDLLHOOKSTRUCT_FLAGS(LLKHF_INJECTED) == KBDLLHOOKSTRUCT_FLAGS(0) {
                if vk == VK_LWIN || vk == VK_RWIN {
                    if down {
                        WIN_DOWN.store(vk, Ordering::Relaxed);
                        WIN_COMBO.store(false, Ordering::Relaxed);
                    } else if msg == WM_KEYUP || msg == WM_SYSKEYUP {
                        let was = WIN_DOWN.swap(0, Ordering::Relaxed);
                        let bare = was != 0 && !WIN_COMBO.load(Ordering::Relaxed);
                        // 先放行：抬起事件必须真实送达，修饰位才能正常复位
                        let ret = CallNextHookEx(None, code, w, l);
                        // Win 键接管关闭时：不吞、不弹，完全交给系统开始菜单
                        if bare && crate::settings::start_hook() {
                            send_dummy_key();
                            let _ = PostMessageW(hwnd(), WM_START_TOGGLE, WPARAM(0), LPARAM(0));
                        }
                        return ret;
                    }
                } else if down && WIN_DOWN.load(Ordering::Relaxed) != 0 {
                    WIN_COMBO.store(true, Ordering::Relaxed);
                }
            }
        }
    }
    CallNextHookEx(None, code, w, l)
}

/// 发送一个未分配键（VK 0xE8）的按下+抬起。Win 键抬起后立刻出现这个键，
/// 系统就认定"Win 被用在组合里"，shell 不再弹开始菜单。0xE8 无任何系统
/// 或程序绑定，应用层收到也只是空事件，无副作用。
fn send_dummy_key() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
    };
    const VK_NONE: u16 = 0xE8;
    unsafe {
        let mut inputs: [INPUT; 2] = [Default::default(); 2];
        inputs[0].r#type = INPUT_KEYBOARD;
        inputs[0].Anonymous.ki.wVk = VIRTUAL_KEY(VK_NONE);
        inputs[1].r#type = INPUT_KEYBOARD;
        inputs[1].Anonymous.ki.wVk = VIRTUAL_KEY(VK_NONE);
        inputs[1].Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;
        let _ = SendInput(
            &inputs,
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}

fn unhook() {
    for slot in [&MOUSE_HOOK, &KEY_HOOK] {
        let hk = slot.swap(0, Ordering::Relaxed);
        if hk != 0 {
            unsafe {
                let _ = UnhookWindowsHookEx(HHOOK(hk as *mut _));
            }
        }
    }
}

/// 真正收起（窗口线程）：藏窗 + 复位状态（钩子是常驻的，不摘）
fn close_window(h: HWND, m: &mut Menu) {
    OPEN.store(false, Ordering::Relaxed);
    m.power_open = false;
    m.hover_tile = None;
    m.hover_btn = None;
    m.hover_power = None;
    m.press_tile = None;
    m.dragging = false;
    m.drag_target = None;
    DRAG_ACTIVE.store(false, Ordering::Relaxed);
    m.mouse_tracked = false;
    unsafe {
        let _ = ShowWindow(h, SW_HIDE);
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CREATE {
            let cs = *(l.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(h, GWLP_USERDATA, cs.lpCreateParams as isize);
            return LRESULT(0);
        }
        let ptr = GetWindowLongPtrW(h, GWLP_USERDATA) as *mut Menu;
        if ptr.is_null() {
            return DefWindowProcW(h, msg, w, l);
        }
        let m = &mut *ptr;
        match msg {
            WM_START_OPEN => {
                m.reload();
                m.place(h);
                m.render(h);
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                LRESULT(0)
            }
            WM_START_CLOSE => {
                close_window(h, m);
                LRESULT(0)
            }
            WM_START_TOGGLE => {
                toggle();
                LRESULT(0)
            }
            WM_START_DROP => {
                let (x, y) = decode_xy(l);
                let k = m.scale(h);
                m.finish_drag(h, x as f32 / k, y as f32 / k);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = decode_xy(l);
                let k = m.scale(h);
                let (lx, ly) = (x as f32 / k, y as f32 / k);
                let hit = |rs: &[[f32; 4]]| {
                    rs.iter().position(|r| {
                        lx >= r[0] && lx < r[0] + r[2] && ly >= r[1] && ly < r[1] + r[3]
                    })
                };
                m.hover_tile = hit(&m.tiles);
                m.hover_btn = hit(&m.buttons);
                m.hover_power = if m.power_open { hit(&m.power_rows) } else { None };
                // 按住左键拖动图标：移动越过阈值后实时换位，松手才写回设置
                if m.press_tile.is_some() && (w.0 & MK_LBUTTON) != 0 {
                    let dx = lx - m.press_pos.0;
                    let dy = ly - m.press_pos.1;
                    if !m.dragging && dx * dx + dy * dy >= 36.0 {
                        m.dragging = true;
                        DRAG_ACTIVE.store(true, Ordering::Relaxed);
                    }
                    if m.dragging {
                        m.drag_target = hit(&m.tiles);
                        if let Some(dst) = m.drag_target {
                            let src = m.press_tile.unwrap_or(dst);
                            m.move_pin(src, dst);
                        }
                    }
                }
                if !m.mouse_tracked {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: h,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    m.mouse_tracked = true;
                }
                m.render(h);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                m.mouse_tracked = false;
                m.hover_tile = None;
                m.hover_btn = None;
                m.hover_power = None;
                m.render(h);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = decode_xy(l);
                let k = m.scale(h);
                let (lx, ly) = (x as f32 / k, y as f32 / k);
                let inside = |r: &[f32; 4]| {
                    lx >= r[0] && lx < r[0] + r[2] && ly >= r[1] && ly < r[1] + r[3]
                };
                // 关机小菜单开着：点行执行动作，点别处先收小菜单再继续命中
                if m.power_open {
                    if let Some(i) = m.power_rows.iter().position(|r| inside(r)) {
                        let action = match i {
                            0 => PowerAction::Shutdown,
                            1 => PowerAction::Restart,
                            _ => PowerAction::Lock,
                        };
                        crate::tlog!("[start] 电源动作 {action:?}");
                        crate::guard::power(action);
                        close_window(h, m);
                        return LRESULT(0);
                    }
                    m.power_open = false;
                    m.power_rows.clear();
                    m.hover_power = None;
                }
                if let Some(i) = m.buttons.iter().position(|r| inside(r)) {
                    match i {
                        0 => {
                            // 关机：弹出 关机/重启/锁屏
                            m.power_open = true;
                            m.layout_power();
                            m.hover_power = None;
                            m.render(h);
                        }
                        1 => {
                            close_window(h, m);
                            crate::settingsui::show();
                        }
                        _ => {
                            close_window(h, m);
                            open_windows_settings();
                        }
                    }
                    return LRESULT(0);
                }
                if let Some(i) = m.tiles.iter().position(|r| inside(r)) {
                    // 先记下按下位置：抬起时若没越过拖动阈值，仍按原来的点击启动处理
                    m.press_tile = Some(i);
                    m.press_pos = (lx, ly);
                    m.dragging = false;
                    m.drag_target = None;
                    return LRESULT(0);
                }
                m.render(h);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = decode_xy(l);
                let k = m.scale(h);
                m.finish_drag(h, x as f32 / k, y as f32 / k);
                LRESULT(0)
            }
            WM_NCHITTEST => {
                // 注意：NCHITTEST 的坐标是**屏幕坐标**，要减去窗口原点换算成
                // 窗口内相对坐标再判圆角——直接拿屏幕坐标比窗口尺寸的话，
                // 菜单在屏幕底部（y 上千）会被判成"全在窗外"，整个窗口返回
                // HTTRANSPARENT，所有按钮都点不到。
                let (x, y) = decode_xy(l);
                let mut r = RECT::default();
                let _ = GetWindowRect(h, &mut r);
                let (lx, ly) = ((x - r.left) as f32, (y - r.top) as f32);
                let (ww, hh) = m.size;
                if inside_round(lx, ly, ww as f32, hh as f32, RADIUS) {
                    LRESULT(HTCLIENT as isize)
                } else {
                    LRESULT(HTTRANSPARENT as isize)
                }
            }
            WM_DESTROY => {
                unhook();
                DefWindowProcW(h, msg, w, l)
            }
            _ => DefWindowProcW(h, msg, w, l),
        }
    }
}

fn decode_xy(l: LPARAM) -> (i32, i32) {
    (
        (l.0 & 0xFFFF) as u16 as i16 as i32,
        ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
    )
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

/// 打开 Windows 系统设置
fn open_windows_settings() {
    let args: Vec<u16> = "ms-settings:\0".encode_utf16().collect();
    unsafe {
        let _ = windows::Win32::UI::Shell::ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(args.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
    }
}

/// 启动一个快捷方式（exe / .lnk 都交给 ShellExecuteW）
fn launch(target: &str) {
    // `@admin:` 前缀 = 以管理员权限启动（ShellExecute 的 runas 动词，会弹 UAC）
    let (verb, path) = match target.strip_prefix("@admin:") {
        Some(p) => (w!("runas"), p),
        None => (w!("open"), target),
    };
    let mut file: Vec<u16> = path.encode_utf16().collect();
    file.push(0);
    unsafe {
        let _ = windows::Win32::UI::Shell::ShellExecuteW(
            HWND::default(),
            verb,
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
    }
}

/// 取图标用的真实路径（剥离 @admin: 前缀）
fn icon_path(target: &str) -> &str {
    target.strip_prefix("@admin:").unwrap_or(target)
}

impl Menu {
    fn scale(&self, h: HWND) -> f32 {
        (unsafe { GetDpiForWindow(h) } as f32 / 96.0).max(0.5)
    }

    /// 打开时刷新数据：重读快捷方式列表，缺图标的补上
    fn reload(&mut self) {
        let mut pins = crate::settings::start_pins();
        pins.truncate(max_tiles());
        for (_, target) in &pins {
            if self.icons.contains_key(target) {
                continue;
            }
            let rgba = crate::dock::large_icon_of_path(std::path::Path::new(icon_path(target)), 64);
            if !rgba.is_empty() {
                self.icons.insert(target.clone(), rgba);
            }
        }
        // 列表里已经删掉的图标顺手清掉，别让缓存一直涨
        self.icons.retain(|k, _| pins.iter().any(|(_, t)| t == k));
        self.pins = pins;
        self.layout();
    }

    /// 计算逻辑布局（格子 / 按钮 / 窗口逻辑高度）
    fn layout(&mut self) {
        let cols = crate::settings::start_cols().max(1);
        // 每行数量可配：tile 宽度按菜单宽度均分，高度随宽度等比缩放，
        // 保证列数变了也能整齐铺满、窗口宽度不变（点击命中用同一组 tiles）
        let tile_w = ((MENU_W - PAD * 2.0 - (cols as f32 - 1.0) * TILE_GAP) / cols as f32)
            .max(48.0);
        let tile_h = tile_w; // 保持正方形格子
        let n = self.pins.len().min(max_tiles());
        let rows = ((n + cols - 1) / cols).max(1);
        let grid_h = (rows as f32) * tile_h + (rows.saturating_sub(1)) as f32 * TILE_GAP;
        self.menu_h = PAD + grid_h + GRID_BAR_GAP + 1.0 + GRID_BAR_GAP + BAR_H;

        let mut tiles = Vec::new();
        for (i, _) in self.pins.iter().enumerate() {
            let (col, row) = (i % cols, i / cols);
            let x = PAD + col as f32 * (tile_w + TILE_GAP);
            let y = PAD + row as f32 * (tile_h + TILE_GAP);
            tiles.push([x, y, tile_w, tile_h]);
        }
        let bar_y = self.menu_h - BAR_H;
        let bw = MENU_W / 3.0;
        let buttons: Vec<[f32; 4]> =
            (0..3).map(|i| [i as f32 * bw, bar_y, bw, BAR_H]).collect();

        self.tiles = tiles;
        self.buttons = buttons;
        if self.power_open {
            self.layout_power();
        }
    }

    /// 拖动中把 src 格子的图标移到 dst（格子矩形是固定位置，pins 换序即完成换位）
    fn move_pin(&mut self, src: usize, dst: usize) {
        if src == dst || src >= self.pins.len() || dst >= self.pins.len() {
            return;
        }
        let item = self.pins.remove(src);
        self.pins.insert(dst, item);
        self.press_tile = Some(dst);
    }

    /// 左键抬起统一收尾：拖过 → 写回设置；没拖过 → 点击启动。
    /// 窗口自己的 WM_LBUTTONUP 与钩子投递的 WM_START_DROP 都会走这里，take() 保证幂等。
    fn finish_drag(&mut self, h: HWND, lx: f32, ly: f32) {
        let Some(idx) = self.press_tile.take() else {
            return;
        };
        if self.dragging {
            // 最后再对齐一次目标格子：避免最后一次 MOUSEMOVE 没送到就松手
            if let Some(dst) = self.tiles.iter().position(|r| {
                lx >= r[0] && lx < r[0] + r[2] && ly >= r[1] && ly < r[1] + r[3]
            }) {
                self.move_pin(idx, dst);
            }
            // 只改显示出来的前几项，设置里若还有额外条目保持原样
            let mut full = crate::settings::start_pins();
            for (slot, pin) in full.iter_mut().enumerate().take(self.pins.len()) {
                *pin = self.pins[slot].clone();
            }
            crate::settings::set_start_pins(full);
            crate::tlog!("[start] 快捷方式顺序已保存");
        } else {
            // 没拖动 = 普通点击：必须在按下的那个格子上松开才启动
            let still = self.tiles.get(idx).map_or(false, |r| {
                lx >= r[0] && lx < r[0] + r[2] && ly >= r[1] && ly < r[1] + r[3]
            });
            if still {
                if let Some((_, target)) = self.pins.get(idx) {
                    crate::tlog!("[start] 启动 {target}");
                    launch(target);
                }
            }
        }
        self.dragging = false;
        self.drag_target = None;
        DRAG_ACTIVE.store(false, Ordering::Relaxed);
        close_window(h, self);
    }

    /// 关机小菜单贴在下栏上方（窗口内），默认对齐关机按钮
    fn layout_power(&mut self) {
        let bar_y = self.menu_h - BAR_H;
        let top = (bar_y - 6.0 - POWER_ROW_H * 3.0 - POWER_PAD * 2.0).max(4.0);
        let mut rows = Vec::new();
        for i in 0..3 {
            rows.push([
                PAD + 4.0,
                top + POWER_PAD + i as f32 * POWER_ROW_H,
                POWER_W - 8.0,
                POWER_ROW_H,
            ]);
        }
        self.power_rows = rows;
        self.power_x = PAD + 4.0;
        self.power_y = top;
    }

    fn place(&mut self, h: HWND) {
        let k = self.scale(h);
        let w = (MENU_W * k).round() as i32;
        let hh = (self.menu_h * k).round() as i32;
        self.size = (w, hh);
        let mon = crate::window::primary_monitor();
        // 菜单在屏幕上水平、垂直都居中（不再贴 Dock 上沿）
        let x = mon.rcMonitor.left
            + ((mon.rcMonitor.right - mon.rcMonitor.left - w) / 2).max(0);
        let y = mon.rcMonitor.top
            + ((mon.rcMonitor.bottom - mon.rcMonitor.top - hh) / 2).max(8);
        unsafe {
            let _ = SetWindowPos(h, HWND_TOPMOST, x, y, w, hh, SWP_NOACTIVATE);
        }
    }

    fn render(&mut self, h: HWND) {
        let (w, hh) = self.size;
        if w <= 0 || hh <= 0 || !OPEN.load(Ordering::Relaxed) {
            return;
        }
        let k = self.scale(h);
        let rgba = paint(
            &mut self.soft,
            k,
            w,
            hh,
            &self.pins,
            &self.icons,
            &self.tiles,
            &self.buttons,
            &self.power_rows,
            (self.power_x, self.power_y),
            (
                self.power_open,
                self.hover_tile,
                self.hover_btn,
                self.hover_power,
            ),
        );
        let bgra = render::premultiplied_rgba_to_bgra(&rgba);
        render::present(h, &bgra, w, hh);
    }
}

// ---------- 绘制 ----------

/// 按钮条上的三个矢量小图标 + 文本标签
const BAR_LABELS: [&str; 3] = ["关机", "设置", "系统"];
const POWER_LABELS: [&str; 3] = ["关机", "重启", "锁屏"];

fn draw_power_icon(p: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    // 竖线 + 圆环（经典电源符号）
    p.line_segment(
        [c + egui::vec2(0.0, -9.0), c + egui::vec2(0.0, -1.5)],
        egui::Stroke::new(2.0, color),
    );
    p.circle_stroke(c, 6.5, egui::Stroke::new(2.0, color));
}

fn draw_gear_icon(p: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    p.circle_stroke(c, 5.2, egui::Stroke::new(1.6, color));
    p.circle_filled(c, 2.0, color);
    for i in 0..8 {
        let a = (i as f32) * std::f32::consts::TAU / 8.0;
        let d = egui::vec2(a.cos(), a.sin());
        p.line_segment([c + d * 6.2, c + d * 8.8], egui::Stroke::new(1.6, color));
    }
}

#[allow(clippy::too_many_arguments)]
fn paint(
    soft: &mut SoftBackend,
    k: f32,
    w: i32,
    h: i32,
    pins: &[(String, String)],
    icons: &HashMap<String, Vec<u8>>,
    tiles: &[[f32; 4]],
    buttons: &[[f32; 4]],
    power_rows: &[[f32; 4]],
    (power_x, power_y): (f32, f32),
    (power_open, hover_tile, hover_btn, hover_power): (bool, Option<usize>, Option<usize>, Option<usize>),
) -> Vec<u8> {
    soft.set_scale(k);
    let fw = w as f32 / k;
    let fh = h as f32 / k;
    let ctx = soft.ctx().clone();
    let input = soft.input(fw, fh);
    let mut out = ctx.run_ui(input, |ui| {
        let p = ui.painter().clone();
        let t = cfg::cur();
        // 底板
        let board = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(fw, fh));
        glass_board(&p, board, egui::CornerRadius::same(RADIUS as u8));

        // ---- 上栏：快捷方式格子 ----
        for (i, (name, target)) in pins.iter().enumerate() {
            let Some(r) = tiles.get(i) else { continue };
            let rect = egui::Rect::from_min_size(
                egui::pos2(r[0], r[1]),
                egui::vec2(r[2], r[3]),
            );
            if Some(i) == hover_tile {
                p.rect_filled(rect, egui::CornerRadius::same(10), argb(t.hover));
            }
            let cx = rect.center().x;
            let k = rect.height() / TILE; // 相对默认 88 高的缩放，列数变化时图标/文字等比
            let icon_center = egui::pos2(cx, rect.min.y + 34.0 * k);
            let icon_side = 42.0 * k.min(1.2);
            if let Some(rgba) = icons.get(target) {
                let px = ((rgba.len() / 4) as f32).sqrt().round().max(1.0) as usize;
                if px * px * 4 <= rgba.len() {
                    let tex = soft.soft_image(&rgba[..px * px * 4], px, px);
                    p.image(
                        tex.id(),
                        egui::Rect::from_center_size(icon_center, egui::vec2(icon_side, icon_side)),
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
            } else {
                // 没抓到图标：圆角色块 + 首字占位
                p.rect_filled(
                    egui::Rect::from_center_size(icon_center, egui::vec2(icon_side, icon_side)),
                    egui::CornerRadius::same(10),
                    argb(with_alpha(t.accent, 0.25)),
                );
                let first: String = name.chars().take(1).collect();
                p.text(
                    icon_center,
                    egui::Align2::CENTER_CENTER,
                    &first,
                    egui::FontId::proportional(20.0),
                    argb(t.text),
                );
            }
            let label = ellipsize(soft, name, rect.width() - 6.0, 12.0);
            p.text(
                egui::pos2(cx, rect.min.y + 64.0 * k),
                egui::Align2::CENTER_CENTER,
                &label,
                egui::FontId::proportional(12.0),
                argb(t.text_body),
            );
        }

        // ---- 分隔线 ----
        let bar_y = fh - BAR_H;
        let div_y = bar_y - GRID_BAR_GAP + 0.5;
        p.line_segment(
            [egui::pos2(PAD, div_y), egui::pos2(fw - PAD, div_y)],
            egui::Stroke::new(1.0, argb(t.divider)),
        );

        // ---- 下栏：关机 / 设置 / 系统 ----
        for (i, label) in BAR_LABELS.iter().enumerate() {
            let Some(r) = buttons.get(i) else { continue };
            let rect = egui::Rect::from_min_size(
                egui::pos2(r[0] + 5.0, r[1] + 5.0),
                egui::vec2(r[2] - 10.0, r[3] - 10.0),
            );
            let hovered = Some(i) == hover_btn;
            if hovered {
                p.rect_filled(rect, egui::CornerRadius::same(10), argb(t.hover));
            }
            let cx = rect.center().x;
            let icon_color = argb(if hovered { t.text } else { t.text_dim });
            let icon_center = egui::pos2(cx, rect.min.y + 20.0);
            match i {
                0 => draw_power_icon(&p, icon_center, icon_color),
                _ => draw_gear_icon(&p, icon_center, icon_color),
            }
            p.text(
                egui::pos2(cx, rect.min.y + 38.0),
                egui::Align2::CENTER_CENTER,
                *label,
                egui::FontId::proportional(12.0),
                argb(t.text_body),
            );
        }

        // ---- 关机小菜单（卡片贴在下栏上方）----
        if power_open {
            let card_w = POWER_W;
            let card_h = POWER_ROW_H * 3.0 + POWER_PAD * 2.0;
            let card = egui::Rect::from_min_size(
                egui::pos2(power_x, power_y),
                egui::vec2(card_w, card_h),
            );
            // 实底 + 细描边 + 一点柔光，盖住下面的格子。
            // 底色跟着「界面透明度」走，但比板面再深一档（+40），否则底下的格子会透上来。
            let card_bg = cfg::bg_alpha(t.panel_bg);
            let card_a = (((card_bg >> 24) + 40).min(255) << 24) | (card_bg & 0x00FF_FFFF);
            p.rect_filled(card, egui::CornerRadius::same(10), argb(card_a));
            p.rect_stroke(
                card,
                egui::CornerRadius::same(10),
                egui::Stroke::new(1.0, argb(t.panel_border)),
                egui::StrokeKind::Inside,
            );
            for (i, label) in POWER_LABELS.iter().enumerate() {
                let Some(r) = power_rows.get(i) else { continue };
                let row = egui::Rect::from_min_size(
                    egui::pos2(r[0], r[1]),
                    egui::vec2(r[2], r[3]),
                );
                if Some(i) == hover_power {
                    p.rect_filled(row, egui::CornerRadius::same(8), argb(t.hover));
                }
                let icon_color = argb(if Some(i) == hover_power { t.text } else { t.text_dim });
                draw_power_icon(&p, egui::pos2(row.min.x + 16.0, row.center().y), icon_color);
                p.text(
                    egui::pos2(row.min.x + 32.0, row.center().y),
                    egui::Align2::LEFT_CENTER,
                    *label,
                    egui::FontId::proportional(13.0),
                    argb(t.text_body),
                );
            }
        }
    });
    soft.update_textures(&mut out.textures_delta);
    soft.rasterize(out.shapes, out.pixels_per_point, w, h)
}
