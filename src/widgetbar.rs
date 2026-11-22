//! 分离出去的模块条：贴在顶部胶囊**左侧或右侧**的一条（或几条）独立分层小窗。
//! 内容和胶囊里的 widget 完全一样（天气 / 邮件 / 剪贴板），只是搬到了外面——
//! 和音乐条一个路子：同高、同底色、圆角胶囊形，视觉上像胶囊分出来的一段。
//!
//! 摆放由 `settings.txt` 的 `weather_pos` / `mail_pos` / `clipboard_pos` 决定：
//! - `pill`              留在胶囊里
//! - `left` / `right`    分离到某一侧的第 1 条（= `left1` / `right1`）
//! - `left2` / `left3` / `right2` / `right3` 同侧的第 2、3 条
//!
//! **同侧同编号合并成一条，编号不同就各自一条胶囊**（1 号最贴近胶囊，往外递增）。
//! 每侧最多 `config::WIDGET_BAR_MAX_SLOT` 条，窗口按 (侧, 编号) 一一对应，懒创建。
//!
//! 整窗默认鼠标穿透，只有 widget 自己的矩形接鼠标；点击不自己处理业务，
//! 而是 `PostMessage` 给胶囊窗口，由 `App::on_widget` 分发。

use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, LoadCursorW, PostMessageW, RegisterClassW, SetWindowPos,
    ShowWindow, HMENU, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW, SWP_NOACTIVATE, SW_HIDE,
    SW_SHOWNOACTIVATE, WM_APP, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCHITTEST, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WNDCLASSW,
};

use crate::config as cfg;
use crate::egui_soft::SoftBackend;
use crate::render::{self, WidgetItem};

/// 点了模块条里的某个 widget：wParam = widget 类型号（见 `window::WidgetKind::id`）
pub const WM_WIDGET_CLICK: u32 = WM_APP + 0x31;

/// 每侧最多几条
const SLOTS: usize = cfg::WIDGET_BAR_MAX_SLOT as usize;
/// 窗口总数：左右各 SLOTS 条
const BARS: usize = SLOTS * 2;

/// 条贴在胶囊的哪一侧
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    fn idx(self) -> usize {
        match self {
            Side::Left => 0,
            Side::Right => 1,
        }
    }
}

thread_local! {
    /// 每个 (侧, 编号) 一条窗口，懒创建
    static WIN: [Cell<isize>; BARS] = [const { Cell::new(0) }; BARS];
    static INSTANCE: Cell<isize> = const { Cell::new(0) };
    /// 胶囊窗口（点击事件回传给它）
    static OWNER: Cell<isize> = const { Cell::new(0) };
    /// 这个槽位当前代表哪一侧的哪一条：(1=左 / 2=右, 编号)，(0, 0) = 这条没在用
    static KEY: [Cell<(u8, u8)>; BARS] = [const { Cell::new((0, 0)) }; BARS];
    /// 条里每个 widget 的 (类型号, 内容)
    static ITEMS: [RefCell<Vec<(u8, WidgetItem)>>; BARS] =
        [const { RefCell::new(Vec::new()) }; BARS];
    /// 每个 widget 的命中矩形（client 坐标）
    static RECTS: [RefCell<Vec<[f32; 4]>>; BARS] =
        [const { RefCell::new(Vec::new()) }; BARS];
    /// 当前悬停的 widget 下标（-1 = 没有）
    static HOVER: [Cell<isize>; BARS] = [const { Cell::new(-1) }; BARS];
    static SCALE: [Cell<f32>; BARS] = [const { Cell::new(1.0) }; BARS];
    /// 条的屏幕矩形（隐藏时宽高为 0）
    static RECT: [Cell<(i32, i32, i32, i32)>; BARS] =
        [const { Cell::new((0, 0, 0, 0)) }; BARS];
    /// 上一次真正画出去的内容键 + 悬停 + 矩形：都一样就不再重画
    static SHOWN: [RefCell<Option<(String, isize, (i32, i32, i32, i32))>>; BARS] =
        [const { RefCell::new(None) }; BARS];
    static TRACKED: [Cell<bool>; BARS] = [const { Cell::new(false) }; BARS];
    /// 每条窗口的 egui 软件后端（懒初始化：Option 首帧才 new）
    static VIEW: [RefCell<Option<SoftBackend>>; BARS] =
        [const { RefCell::new(None) }; BARS];
}

/// 用第 i 条窗口的 egui 后端画一帧，并把预乘 RGBA 上屏。
/// 返回 widget 命中矩形（物理像素）。界面透明度只作用在板面底色上（`cfg::ui_bg_effective`）。
fn draw_bar(i: usize, hw: HWND, w: i32, h: i32, scale: f32, widgets: &[WidgetItem], hovered: Option<usize>) -> Vec<[f32; 4]> {
    VIEW.with(|c| {
        let mut slot = c[i].borrow_mut();
        if slot.is_none() {
            *slot = Some(SoftBackend::new());
        }
        let soft = slot.as_mut().unwrap();
        let out = crate::widgetbarview::paint(soft, w, h, scale, widgets, hovered);
        if !out.rgba.is_empty() {
            // egui 软件后端输出已是预乘 RGBA；透明度已在画底色时按滑条压过 alpha
            let bgra = render::premultiplied_rgba_to_bgra(&out.rgba);
            render::present(hw, &bgra, w, h);
        }
        out.rects
    })
}

/// 一帧里的某一条：同侧同 slot 的 widget 已经被 `window.rs` 合成一组
pub struct BarView<'a> {
    pub side: Side,
    /// 同侧的分组编号（1 = 最贴近胶囊）
    pub slot: u8,
    /// (widget 类型号, 内容)，按显示顺序
    pub items: &'a [(u8, WidgetItem)],
}

/// 一帧的全部模块条
pub struct Frame<'a> {
    pub bars: &'a [BarView<'a>],
    pub scale: f32,
    /// 胶囊的屏幕矩形 (x, y, w, h)
    pub pill: (i32, i32, i32, i32),
    /// 整体透明度（与胶囊一致，烘焙进像素）
    pub alpha: f32,
}

pub fn init(instance: HINSTANCE, owner: HWND) {
    INSTANCE.with(|c| c.set(instance.0 as isize));
    OWNER.with(|c| c.set(owner.0 as isize));
    unsafe {
        let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: arrow,
            lpszClassName: w!("ORBIT_WIDGETBAR"),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

fn hwnd_at(i: usize) -> HWND {
    HWND(WIN.with(|c| c[i].get()) as *mut _)
}

/// 窗口句柄 → 槽位下标（同一个窗口类、同一个过程）
fn index_of(h: HWND) -> Option<usize> {
    let v = h.0 as isize;
    WIN.with(|c| (0..BARS).find(|&i| c[i].get() == v))
}

/// 用到哪条才创建哪条（大多数配置只会用到 1~2 条）
fn ensure(i: usize) -> HWND {
    let h = hwnd_at(i);
    if !h.is_invalid() {
        return h;
    }
    unsafe {
        let inst = HINSTANCE(INSTANCE.with(|c| c.get()) as *mut _);
        if let Ok(h) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("ORBIT_WIDGETBAR"),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            HWND::default(),
            HMENU::default(),
            inst,
            None,
        ) {
            WIN.with(|c| c[i].set(h.0 as isize));
            h
        } else {
            HWND::default()
        }
    }
}

/// 屏幕矩形是否落在某条模块条里（面板自动收起要把这里算进"内部"）
pub fn contains(pt: POINT) -> bool {
    (0..BARS).any(|i| {
        let (x, y, w, h) = RECT.with(|c| c[i].get());
        w > 0 && h > 0 && pt.x >= x && pt.x <= x + w && pt.y >= y && pt.y <= y + h
    })
}

/// 某个 widget 当前在屏幕上的水平中心（用于面板锚点）；不在条里返回 None
pub fn widget_center(kind: u8) -> Option<i32> {
    (0..BARS).find_map(|i| {
        let (wx, _, _, _) = RECT.with(|c| c[i].get());
        let rects = RECTS.with(|c| c[i].borrow().clone());
        let items = ITEMS.with(|c| c[i].borrow().clone());
        let idx = items.iter().position(|(k, _)| *k == kind)?;
        let r = rects.get(idx)?;
        Some(wx + (r[0] + r[2] / 2.0) as i32)
    })
}

/// 某一侧的"最外沿"矩形（多条时取离胶囊最远的那条），供音乐条避让
pub fn rect(side: Side) -> Option<(i32, i32, i32, i32)> {
    let mut out: Option<(i32, i32, i32, i32)> = None;
    for k in 0..SLOTS {
        let (x, y, w, h) = RECT.with(|c| c[side.idx() * SLOTS + k].get());
        if w <= 0 || h <= 0 {
            continue;
        }
        out = Some(match out {
            None => (x, y, w, h),
            Some(p) => match side {
                // 左条：往外 = 更靠左
                Side::Left if x < p.0 => (x, y, w, h),
                // 右条：往外 = 更靠右
                Side::Right if x + w > p.0 + p.2 => (x, y, w, h),
                _ => p,
            },
        });
    }
    out
}

/// 某个条当前是否可见（按侧取最外沿）
#[allow(dead_code)]
pub fn visible(side: Side) -> bool {
    rect(side).is_some()
}

/// 按一帧的布局摆好所有条：该显示的显示、该埋的埋、位置一条条从胶囊往外排。
pub fn sync(f: &Frame) {
    let m = crate::window::primary_monitor().rcMonitor;
    // 模块条与胶囊等高、同字号：胶囊大小设置也作用于这里
    let s = (f.scale * crate::settings::pill_scale()).max(0.5);
    let gap = (cfg::WIDGET_BAR_GAP as f32 * s).round() as i32;
    let hh = (cfg::PILL_H * s).ceil() as i32;
    let (px, py, pw, _) = f.pill;
    // 贴边模式的胶囊窗口比条身左右各宽一只"耳"：模块条按条身算间隙
    let ear = crate::render::ear_w_phys(s);
    let min_x = m.left + 6;
    let max_x = m.right - 6;

    for side in [Side::Left, Side::Right] {
        let mut list: Vec<&BarView> = f
            .bars
            .iter()
            .filter(|b| b.side == side && !b.items.is_empty())
            .collect();
        // 编号小的贴近胶囊
        list.sort_by_key(|b| b.slot);

        // 先量好各自的宽度，才知道整排占多宽
        let mut widths = Vec::with_capacity(list.len());
        for b in &list {
            let items: Vec<WidgetItem> = b.items.iter().map(|(_, it)| it.clone()).collect();
            // 文本宽度（验证码 / "新邮件"）要用 SoftBackend 的度量通道实测，
            // 才能随内容自适应变长。复用槽位自己的后端（懒创建），并先 set_scale：
            // 度量用的字体/像素比和 draw_bar 画的时候保持一致，量出来才准。
            let i = side.idx() * SLOTS + (b.slot.clamp(1, cfg::WIDGET_BAR_MAX_SLOT) as usize - 1);
            VIEW.with(|c| {
                let mut slot = c[i].borrow_mut();
                if slot.is_none() {
                    *slot = Some(SoftBackend::new());
                }
                let soft = slot.as_mut().unwrap();
                soft.set_scale(s);
                widths.push((crate::widgetbarview::bar_width(soft, &items) * s).ceil() as i32);
            });
        }
        let total: i32 = widths.iter().sum::<i32>() + gap * (list.len() as i32 - 1).max(0);

        // 从胶囊往外排：左条编号大的在最左边（所以倒序摆放），右条正序
        let mut xs = vec![0i32; list.len()];
        match side {
            Side::Left => {
                let mut x = (px + ear - gap - total).max(min_x);
                for k in (0..list.len()).rev() {
                    xs[k] = x;
                    x += widths[k] + gap;
                }
            }
            Side::Right => {
                let mut x = px + pw - ear + gap;
                if x + total > max_x {
                    x = (max_x - total).max(min_x);
                }
                for k in 0..list.len() {
                    xs[k] = x;
                    x += widths[k] + gap;
                }
            }
        }

        let mut used = [false; SLOTS];
        for (k, b) in list.iter().enumerate() {
            let slot = b.slot.clamp(1, cfg::WIDGET_BAR_MAX_SLOT);
            let i = side.idx() * SLOTS + (slot - 1) as usize;
            if used[i - side.idx() * SLOTS] {
                // 同侧同编号给了两组（window.rs 应该已经合并过），后面的丢掉并留痕
                crate::tlog!("[widgetbar] 同侧同编号重复 slot={slot}，忽略后一组");
                continue;
            }
            used[i - side.idx() * SLOTS] = true;
            show(i, side, slot, b.items, s, (xs[k], py, widths[k], hh), f.alpha);
        }
        for k in 0..SLOTS {
            if !used[k] {
                hide(side.idx() * SLOTS + k);
            }
        }
    }
}

fn show(
    i: usize,
    side: Side,
    slot: u8,
    items: &[(u8, WidgetItem)],
    scale: f32,
    rect: (i32, i32, i32, i32),
    alpha: f32,
) {
    let (x, y, w, h) = rect;
    if w <= 0 || h <= 0 {
        hide(i);
        return;
    }
    let hw = ensure(i);
    if hw.is_invalid() {
        return;
    }
    let plain: Vec<WidgetItem> = items.iter().map(|(_, it)| it.clone()).collect();

    // 内容键：内容 / 悬停 / 矩形 / 透明度任一变化就重画
    let mut key = format!("{}:{}:a{}", side.idx(), slot, (alpha * 100.0).round() as i32);
    for (k, it) in items {
        key.push_str(&format!("|{k}:{:x}:{}:{}", it.icon_argb, it.icon, it.text));
    }
    let hovered = HOVER.with(|c| c[i].get());

    SCALE.with(|c| c[i].set(scale));
    ITEMS.with(|c| *c[i].borrow_mut() = items.to_vec());
    RECT.with(|c| c[i].set(rect));
    KEY.with(|c| c[i].set((side.idx() as u8 + 1, slot)));

    unsafe {
        let same = SHOWN.with(|s| {
            matches!(&*s[i].borrow(), Some((k, hv, rc)) if k == &key && *hv == hovered && *rc == rect)
        });
        if !same {
            let hv = if hovered >= 0 { Some(hovered as usize) } else { None };
            let rects = draw_bar(i, hw, w, h, scale, &plain, hv);
            RECTS.with(|c| *c[i].borrow_mut() = rects);
            SHOWN.with(|s| *s[i].borrow_mut() = Some((key, hovered, rect)));
        } else if RECTS.with(|c| c[i].borrow().is_empty()) {
            // 命中表丢了（比如第一次显示就被去重），补画一次把布局拿回来
            repaint(i);
        }
        let _ = SetWindowPos(hw, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
        let _ = ShowWindow(hw, SW_SHOWNOACTIVATE);
    }
}

fn hide(i: usize) {
    let h = hwnd_at(i);
    if RECT.with(|c| c[i].get()) == (0, 0, 0, 0) && h.is_invalid() {
        return;
    }
    if !h.is_invalid() {
        unsafe {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
    ITEMS.with(|c| c[i].borrow_mut().clear());
    RECTS.with(|c| c[i].borrow_mut().clear());
    RECT.with(|c| c[i].set((0, 0, 0, 0)));
    KEY.with(|c| c[i].set((0, 0)));
    SHOWN.with(|s| *s[i].borrow_mut() = None);
    HOVER.with(|c| c[i].set(-1));
    // 窗口藏起来后 TrackMouseEvent 的注册就失效了，下次显示要重新注册
    TRACKED.with(|c| c[i].set(false));
}

/// 鼠标在条内的哪个 widget 上（client 坐标）
fn index_at(i: usize, x: f32, y: f32) -> Option<usize> {
    RECTS.with(|c| {
        c[i].borrow()
            .iter()
            .position(|r| x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3])
    })
}

/// 悬停变化后按已存的内容重画（不需要胶囊窗口参与）
fn repaint(i: usize) {
    let h = hwnd_at(i);
    if h.is_invalid() {
        return;
    }
    let items = ITEMS.with(|c| c[i].borrow().clone());
    if items.is_empty() {
        return;
    }
    let scale = SCALE.with(|c| c[i].get());
    let hovered = HOVER.with(|c| c[i].get());
    let (_, _, w, hh) = RECT.with(|c| c[i].get());
    if w <= 0 || hh <= 0 {
        return;
    }
    let plain: Vec<WidgetItem> = items.into_iter().map(|(_, it)| it).collect();
    let hv = if hovered >= 0 { Some(hovered as usize) } else { None };
    let rects = draw_bar(i, h, w, hh, scale, &plain, hv);
    RECTS.with(|c| *c[i].borrow_mut() = rects);
    // 内容键里的悬停要跟着更新，否则下一次 sync() 会以为还要重画
    SHOWN.with(|s| {
        if let Some((_, hv_slot, _)) = s[i].borrow_mut().as_mut() {
            *hv_slot = hovered;
        }
    });
}

fn post(owner: HWND, kind: u8) {
    if !owner.is_invalid() {
        unsafe {
            let _ = PostMessageW(owner, WM_WIDGET_CLICK, WPARAM(kind as usize), LPARAM(0));
        }
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        let Some(i) = index_of(h) else {
            return DefWindowProcW(h, msg, w, l);
        };
        match msg {
            // 只有 widget 自己的矩形接鼠标，条上其余地方一律穿透
            WM_NCHITTEST => {
                let mut pt = POINT {
                    x: (l.0 & 0xFFFF) as u16 as i16 as i32,
                    y: ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
                };
                let _ = ScreenToClient(h, &mut pt);
                if index_at(i, pt.x as f32, pt.y as f32).is_some() {
                    LRESULT(HTCLIENT as isize)
                } else {
                    LRESULT(HTTRANSPARENT as isize)
                }
            }
            WM_MOUSEMOVE => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                let idx = index_at(i, x as f32, y as f32)
                    .map(|n| n as isize)
                    .unwrap_or(-1);
                // 悬停项变了才重画（每次移动都重画等于白烧 CPU）
                if HOVER.with(|c| c[i].replace(idx)) != idx {
                    let (side, slot) = KEY.with(|c| c[i].get());
                    crate::vlog!("[widgetbar] 悬停 → {idx}（{side}-{slot}）");
                    repaint(i);
                }
                if !TRACKED.with(|c| c[i].replace(true)) {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: h,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                TRACKED.with(|c| c[i].set(false));
                if HOVER.with(|c| c[i].replace(-1)) != -1 {
                    repaint(i);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                if let Some(idx) = index_at(i, x as f32, y as f32) {
                    let kind = ITEMS.with(|c| c[i].borrow().get(idx).map(|(k, _)| *k));
                    if let Some(k) = kind {
                        let (side, slot) = KEY.with(|c| c[i].get());
                        crate::tlog!("[widgetbar] 点击 {side}-{slot} 的 widget #{idx} kind={k}");
                        post(HWND(OWNER.with(|c| c.get()) as *mut _), k);
                    }
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(h, msg, w, l),
        }
    }
}
