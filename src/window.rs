//! 顶部胶囊窗口：置顶分层窗口、命中测试（圆角外穿透）、悬停、时钟/widget 轮询，
//! 以及展开面板的开关与点击分发。托盘图标不在这里（在底部 Dock 右侧）。

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, MOD_ALT, MOD_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetCursorPos, GetWindowLongPtrW, GetWindowRect, KillTimer, PostQuitMessage,
    SetTimer, SetWindowLongPtrW, SetWindowPos, CREATESTRUCTW,
    GWLP_USERDATA, HWND_TOPMOST, SWP_NOACTIVATE, WM_COPYDATA, WM_CREATE,
    WM_DESTROY, WM_DISPLAYCHANGE, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_RBUTTONUP, WM_TIMER,
};

use crate::clipboard::{self, Clipboard};
use crate::clock;
use crate::config as cfg;
use crate::dock::{self, FlashIcon};
use crate::hide;
use crate::lyric::Lyrics;
use crate::mail::{Badge, Mail};
use crate::mailview;
use crate::music::{Cmd as MusicCmd, Music};
use crate::musicbar;
use crate::panel::{self, Content, WM_ROW_CLICK};
use crate::render::{self, PanelRow, PillShape, WidgetItem, PIN_BASE, TAB_BASE};
use crate::settings::{self, LyricPos, ModuleMode, PillAnchor};
use crate::traymsg;
use crate::weather::{self, Weather};
use crate::widgetbar::{self, Side as BarSide, WM_WIDGET_CLICK};

/// 定时器 ID（SetTimer 的 usize 标识）
const T_CLOCK: usize = 1;
/// 原生任务栏隐藏状态的补丁轮询（explorer 重启/换主题会抹掉分层样式）
const T_HIDE: usize = 2;
/// 歌词对齐用的快定时器（200ms）
const T_LYRIC: usize = 4;
/// 封面旋转用的帧定时器（40ms，只在播放时开着）
const T_COVER: usize = 5;
/// 托盘闪烁跟随（Dock 在 70ms 采样真实托盘；这里只比版本号，变了就重绘）
const T_FLASH: usize = 6;

pub struct App {
    hwnd: HWND,
    scale: f32,
    /// egui 软件后端（胶囊绘制 + 交互响应）
    pill_ui: crate::pillui::PillUi,
    /// 上次绘制时的时钟（时:分, 日, 秒——秒只在"带秒"格式下参与比较）
    last_time: (u32, u16, u16),
    hovered_widget: Option<usize>,
    mouse_tracked: bool,
    widget_rects: Vec<[f32; 4]>,
    widget_kinds: Vec<WidgetKind>,
    /// 胶囊右侧正在闪烁的托盘图标（Dock 发布的快照，见 `dock::FlashIcon`）
    flash: Vec<FlashIcon>,
    /// 上次见到的闪烁快照版本号（变了才重绘）
    flash_ver: u64,
    /// 悬停中的闪烁图标下标（相对 flash 的下标）
    hovered_flash: Option<usize>,
    /// 上次左键按下的时刻（判双击）
    last_lclick: Option<std::time::Instant>,
    /// 悬停中的闪烁图标下标（相对 flash 的下标）
    /// 已转发"悬停进入"的闪烁图标下标（离开时要补发 leave）
    flash_hover_sent: Option<usize>,
    pill: (i32, i32),
    /// 胶囊当前的屏幕左上角
    pos: (i32, i32),
    weather: Weather,
    music: Music,
    lyrics: Lyrics,
    clipboard: Clipboard,
    mail: Mail,
    /// 当前展开的面板（None = 无）
    panel: Option<WidgetKind>,
    /// 面板当前标签页（0 = 未读，1 = 全部；只有邮件用）
    panel_tab: usize,
    /// 鼠标连续在面板与胶囊之外的 tick 数（用于自动收起）
    panel_outside: u8,
    /// 解码好的封面（art_ver，px*px*4 RGBA）；空 Vec = 这首没有封面
    cover: Option<(u64, Vec<u8>)>,
    /// 封面当前角度（度）与上次推进的时刻
    angle: f32,
    angle_at: std::time::Instant,
    /// 上一次的曲目（歌名 + 歌手），换曲时把角度归零
    last_track: (String, String),
    /// 40ms 的旋转定时器是否开着
    anim_on: bool,
    /// 上次见过的设置版本号（设置页面改值后靠它触发重绘）
    seen_settings: u64,
}

/// 胶囊里的 widget 类型（决定点击行为与面板内容）
#[derive(Clone, Copy, PartialEq, Debug)]
enum WidgetKind {
    Weather,
    Clipboard,
    Mail,
}

impl WidgetKind {
    /// 跨窗口传递用的类型号（分离出去的模块条点回来时用它认领）
    fn id(self) -> u8 {
        match self {
            WidgetKind::Weather => 1,
            WidgetKind::Clipboard => 2,
            WidgetKind::Mail => 3,
        }
    }

    fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(WidgetKind::Weather),
            2 => Some(WidgetKind::Clipboard),
            3 => Some(WidgetKind::Mail),
            _ => None,
        }
    }
}

/// 按当前配置分好位置的 widget：胶囊里的，和分离到胶囊左右各条模块条的。
/// `bars` 里每一项是「某一侧的第 slot 条」以及它装着的 widget（同侧同 slot 已合并）。
struct Widgets {
    pill: Vec<WidgetItem>,
    pill_kinds: Vec<WidgetKind>,
    bars: Vec<(BarSide, u8, Vec<(u8, WidgetItem)>)>,
}

pub unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CREATE {
            let cs = *(l.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return LRESULT(0);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let app = &mut *ptr;
        match msg {
            WM_TIMER => {
                if w.0 == T_CLOCK {
                    let mut dirty = false;
                    let t = now_parts();
                    if t != app.last_time {
                        app.last_time = t;
                        dirty = true;
                    }
                    let weather_changed = app.weather.tick();
                    let music_changed = app.music.tick();
                    let clip_changed = app.clipboard.tick();
                    let mail_changed = app.mail.tick();
                    if weather_changed || music_changed || clip_changed || mail_changed {
                        dirty = true;
                    }
                    // settings.txt 被改过（手改）就热更新；设置页面改值走版本号
                    let file_changed = settings::reload_if_changed();
                    let v = settings::version();
                    if file_changed || v != app.seen_settings {
                        app.seen_settings = v;
                        dirty = true;
                        if app.panel.is_some() {
                            app.reload_panel();
                        }
                        // 设置页面开着时同步控件状态（手改文件也能跟着动）
                        crate::settingsui::sync();
                    }
                    if dirty {
                        app.render();
                    }
                    // 数据变了就把展开中的对应面板同步刷新
                    let stale = matches!(
                        (app.panel, weather_changed, clip_changed, mail_changed),
                        (Some(WidgetKind::Weather), true, _, _)
                            | (Some(WidgetKind::Clipboard), _, true, _)
                            | (Some(WidgetKind::Mail), _, _, true)
                    );
                    if stale {
                        app.reload_panel();
                    }
                    app.tick_panel();
                } else if w.0 == T_HIDE {
                    // explorer 重启/换主题会把任务栏的分层样式抹掉，顺带补一次
                    hide::ensure_hidden();
                } else if w.0 == T_LYRIC || w.0 == T_COVER {
                    app.tick_bar();
                } else if w.0 == T_FLASH {
                    // Dock 的 70ms 采样在跟着真实托盘闪；这里只比快照版本号
                    if dock::flash_version() != app.flash_ver {
                        app.render();
                    }
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                // 喂给 egui（逻辑点），悬停高亮/点击判定由 egui 响应决定
                let k = app.pill_ui_scale();
                app.pill_ui
                    .input()
                    .pointer_moved(x as f32 / k, y as f32 / k);
                if !app.mouse_tracked {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    app.mouse_tracked = true;
                }
                app.render();
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                app.mouse_tracked = false;
                app.pill_ui.input().pointer_left();
                app.render();
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                // 点胶囊上的 widget 就算"离开设置页面"
                crate::settingsui::hide();
                // 按下事件交给 egui，点击在 paint 后按旧语义分发（按下即触发）
                let k = app.pill_ui_scale();
                app.pill_ui
                    .input()
                    .pointer_button(x as f32 / k, y as f32 / k, true);
                app.render();
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                let k = app.pill_ui_scale();
                app.pill_ui
                    .input()
                    .pointer_button(x as f32 / k, y as f32 / k, false);
                app.render();
                LRESULT(0)
            }
            WM_DISPLAYCHANGE => {
                app.render();
                LRESULT(0)
            }
            WM_HOTKEY if w.0 == 1 => {
                // Ctrl+Alt+O：退出并恢复原生任务栏
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_HOTKEY if w.0 == 2 => {
                // Ctrl+Alt+,：设置页面（再按一次收起）
                crate::settingsui::toggle();
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                // 胶囊上右键 = 设置页面（widget 区域右键也算，避免"点了没反应"）
                crate::settingsui::toggle();
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_ROW_CLICK => {
                app.on_panel_click(w.0 as u32);
                LRESULT(0)
            }
            // 邮件阅读窗的"删除邮件"按钮：WM_COPYDATA(dwData=MAIL_DELETE_MAGIC) 带 UID
            WM_COPYDATA => {
                let cds = l.0 as *const COPYDATASTRUCT;
                if !cds.is_null() && (*cds).dwData == mailview::MAIL_DELETE_MAGIC {
                    let (ptr, n) = ((*cds).lpData as *const u16, (*cds).cbData as usize / 2);
                    let uid = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, n));
                    crate::tlog!("[mail] 删除请求 uid={uid}");
                    if app.mail.delete(&uid) {
                        app.render();
                        app.reload_panel();
                    }
                    LRESULT(1)
                } else {
                    DefWindowProcW(hwnd, msg, w, l)
                }
            }
            // 音乐条的封面按钮：左键播放/暂停，右键下一首
            m if m == musicbar::WM_TOGGLE => {
                app.music.send(MusicCmd::Toggle);
                LRESULT(0)
            }
            m if m == musicbar::WM_NEXT => {
                app.music.send(MusicCmd::Next);
                LRESULT(0)
            }
            // 分离出去的模块条被点击：wParam = widget 类型号
            WM_WIDGET_CLICK => {
                if let Some(kind) = WidgetKind::from_id(w.0 as u8) {
                    app.on_widget(kind);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            scale: 1.0,
            pill_ui: crate::pillui::PillUi::new(),
            last_time: (0, 0, 0),
            hovered_widget: None,
            mouse_tracked: false,
            widget_rects: vec![],
            widget_kinds: vec![],
            flash: Vec::new(),
            flash_ver: 0,
            hovered_flash: None,
            flash_hover_sent: None,
            last_lclick: None,
            pill: (0, 0),
            pos: (0, 0),
            weather: Weather::default(),
            music: Music::spawn(),
            lyrics: Lyrics::default(),
            clipboard: Clipboard::spawn(),
            mail: Mail::default(),
            panel: None,
            panel_tab: 0,
            panel_outside: 0,
            cover: None,
            angle: 0.0,
            angle_at: std::time::Instant::now(),
            last_track: (String::new(), String::new()),
            anim_on: false,
            seen_settings: 0,
        }
    }

    /// 胶囊窗口里"逻辑点 → 物理像素"的实际换算系数：DPI 缩放 × 胶囊大小设置。
    /// 鼠标坐标要按同一个系数折算回逻辑点，否则放大后点击位置会偏。
    fn pill_ui_scale(&self) -> f32 {
        (self.scale * settings::pill_scale()).max(0.5)
    }

    pub fn init(&mut self, hwnd: HWND) {
        settings::init();
        self.seen_settings = settings::version();
        unsafe {
            self.hwnd = hwnd;
            self.scale = GetDpiForWindow(hwnd) as f32 / 96.0;
            self.last_time = now_parts();
            // 退出热键：Ctrl+Alt+O
            let _ = RegisterHotKey(hwnd, 1, MOD_CONTROL | MOD_ALT, 'O' as u32);
            // 设置页面：Ctrl+Alt+,
            let _ = RegisterHotKey(hwnd, 2, MOD_CONTROL | MOD_ALT, 0xBC /* VK_OEM_COMMA */);
        }
    }

    /// 计算布局 → egui 软件光栅化（pillui）→ UpdateLayeredWindow → 置顶定位。
    /// 变化检测在 pillui 里做：纯鼠标路过且无悬停/点击变化的帧不上屏。
    pub fn render(&mut self) {
        let scale = self.scale;
        // 日期与时间合并成一段（格式见设置）：整段一起画
        let clock_text = clock::text();
        let Widgets { pill: pill_items, pill_kinds, bars } = self.build_widgets();

        // 托盘闪烁快照：谁在闪就显示谁（灭帧留空位），跟随 Dock 的真实节奏
        let flash = dock::flash_icons();
        self.flash_ver = dock::flash_version();
        if self.hovered_flash.map(|i| i >= flash.len()).unwrap_or(false) {
            self.hovered_flash = None;
        }
        let flash_views: Vec<render::FlashView<'_>> = flash
            .iter()
            .map(|f| render::FlashView { rgba: &f.rgba, px: f.px, dim: f.dim })
            .collect();

        let shape = match settings::pill_anchor() {
            PillAnchor::Float => PillShape::Float,
            PillAnchor::Island => PillShape::Island,
            PillAnchor::Bottom => PillShape::Bottom,
        };
        let out = self.pill_ui.paint(&crate::pillui::View {
            scale,
            clock: &clock_text,
            items: &pill_items,
            flash: &flash_views,
            hovered_widget: self.hovered_widget,
            hovered_flash: self.hovered_flash,
            shape,
            alpha: settings::pill_alpha(),
        });
        self.hovered_widget = out.hovered_widget;
        self.hovered_flash = out.hovered_flash;
        let events = out.events;
        if out.changed {
            self.pill = (out.w, out.h);
            self.widget_rects = out.widget_rects;
            self.widget_kinds = pill_kinds;
            self.flash = flash;
            // egui 输出是预乘 RGBA；整体透明度连 RGB 一起乘，再换 BGRA 上屏
            let mut rgba = out.rgba;
            let alpha = settings::pill_alpha();
            if alpha < 0.999 {
                let k = alpha.clamp(0.0, 1.0);
                for b in rgba.iter_mut() {
                    *b = ((*b as f32) * k).round() as u8;
                }
            }
            let bgra = render::premultiplied_rgba_to_bgra(&rgba);
            render::present(self.hwnd, &bgra, out.w, out.h);

            // 定位：主屏顶部居中（固定水平位置，不可拖动）；垂直位置由 pill_anchor 决定：
            // 悬浮 = 顶边往下一点，吸顶刘海 = 贴屏幕顶，吸底 = 贴屏幕底
            let mx = primary_monitor().rcMonitor;
            let x = mx.left + (mx.right - mx.left - out.w) / 2;
            let y = match settings::pill_anchor() {
                PillAnchor::Float => mx.top + cfg::TOP_MARGIN,
                PillAnchor::Island => mx.top,
                PillAnchor::Bottom => mx.bottom - out.h,
            };
            self.pos = (x, y);
            unsafe {
                let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, out.w, out.h, SWP_NOACTIVATE);
            }
            // 分离出去的模块条跟着胶囊贴过去（没有内容的那条自动隐藏）
            self.sync_bars(&bars);
            // 胶囊宽度/位置变了（换了 widget、改了字号），音乐条跟着重新贴一次
            self.tick_bar();
        }
        // 交互事件（widget/闪烁图标的点击与悬停进出）按旧语义分发
        for e in events {
            match e {
                crate::pillui::PillEvent::WidgetClick(i) => {
                    if let Some(kind) = self.widget_kinds.get(i).copied() {
                        self.on_widget(kind);
                    }
                }
                crate::pillui::PillEvent::FlashClick(i) => self.click_flash(i),
                crate::pillui::PillEvent::FlashEnter(i) => self.flash_hover_enter(i),
                crate::pillui::PillEvent::FlashLeave => self.flash_hover_leave(),
            }
        }
    }

    /// 分离模块条：同侧同编号的 widget 合成一条，和胶囊同高、断成另外几块；
    /// 编号不同的各自一条，按编号从胶囊往外排。
    fn sync_bars(&self, bars: &[(BarSide, u8, Vec<(u8, WidgetItem)>)]) {
        let pill = (self.pos.0, self.pos.1, self.pill.0, self.pill.1);
        let views: Vec<widgetbar::BarView> = bars
            .iter()
            .map(|(side, slot, items)| widgetbar::BarView {
                side: *side,
                slot: *slot,
                items,
            })
            .collect();
        widgetbar::sync(&widgetbar::Frame {
            bars: &views,
            scale: self.scale,
            pill,
            alpha: settings::pill_alpha(),
        });
    }

    /// widget 该摆在哪：胶囊里 / 左条 / 右条 / 隐藏（由 `module_<name>` 决定）
    fn widget_mode(&self, kind: WidgetKind) -> (ModuleMode, &'static str) {
        match kind {
            WidgetKind::Weather => (settings::module_mode("weather"), "weather"),
            WidgetKind::Mail => (settings::module_mode("mail"), "mail"),
            WidgetKind::Clipboard => (settings::module_mode("clipboard"), "clipboard"),
        }
    }

    /// 组装当前可见的 widget（无数据的不占位），按 `module_order` 排序后
    /// 分到胶囊与左右两侧的模块条。**同侧不再分 1/2/3 号**：模块条按顺序
    /// 从胶囊往外排，每个分离模块单独一条（`slot` = 同侧的第几个，1 起计）。
    fn build_widgets(&self) -> Widgets {
        let mut all: Vec<(WidgetKind, WidgetItem)> = Vec::new();
        if cfg::SHOW_WEATHER {
            if let Some(s) = &self.weather.snap {
                all.push((
                    WidgetKind::Weather,
                    WidgetItem {
                        icon: weather::glyph(s.code, s.day).to_string(),
                        icon_argb: weather_color(s.code, s.day),
                        text: format!("{}°", s.temp),
                        icon_kind: 0,
                    },
                ));
            }
        }
        if cfg::SHOW_MAIL && (!self.mail.items.is_empty() || self.mail.error.is_some()) {
            // 平时只有图标；识别到验证码显示验证码；有未读但没有验证码就提示有新邮件
            let (text, argb) = match self.mail.badge() {
                Badge::Code(code) => (code, cfg::cur().sun),
                Badge::Unread => ("新邮件".to_string(), cfg::cur().accent),
                Badge::None => (String::new(), cfg::cur().icon_dim),
            };
            // 图标用矢量信封（见 egui_soft::widget_icon），`icon` 只用来占格
            all.push((
                WidgetKind::Mail,
                WidgetItem { icon: "✉".to_string(), icon_argb: argb, text, icon_kind: crate::egui_soft::KIND_MAIL },
            ));
        }
        if cfg::SHOW_CLIPBOARD && !self.clipboard.items.is_empty() {
            // 只显示图标，不暴露当前剪贴板内容
            all.push((
                WidgetKind::Clipboard,
                WidgetItem {
                    icon: "▤".to_string(),
                    icon_argb: cfg::cur().icon_dim,
                    text: String::new(),
                    icon_kind: crate::egui_soft::KIND_CLIP,
                },
            ));
        }

        let order = settings::module_order();
        let rank = |kind: WidgetKind| -> usize {
            let name = match kind {
                WidgetKind::Weather => "weather",
                WidgetKind::Mail => "mail",
                WidgetKind::Clipboard => "clipboard",
            };
            order.iter().position(|n| n == name).unwrap_or(usize::MAX)
        };

        let mut out = Widgets { pill: Vec::new(), pill_kinds: Vec::new(), bars: Vec::new() };
        // 排序后再分配：胶囊里的按顺序跟在时钟后面；分离的按顺序从胶囊往外排
        let mut sorted: Vec<(WidgetKind, WidgetItem)> = all;
        sorted.sort_by_key(|(k, _)| rank(*k));
        let (mut n_left, mut n_right) = (0u8, 0u8);
        for (kind, item) in sorted {
            let (mode, _) = self.widget_mode(kind);
            match mode {
                ModuleMode::Pill => {
                    out.pill.push(item);
                    out.pill_kinds.push(kind);
                }
                ModuleMode::BarLeft | ModuleMode::BarRight => {
                    let side = if mode == ModuleMode::BarLeft { BarSide::Left } else { BarSide::Right };
                    let slot = match side {
                        BarSide::Left => { n_left += 1; n_left }
                        BarSide::Right => { n_right += 1; n_right }
                    };
                    // 最多 3 条/侧（与 widgetbar 预建窗口数一致），再多并入最后一条
                    let slot = slot.min(cfg::WIDGET_BAR_MAX_SLOT);
                    match out.bars.iter_mut().find(|(s, sl, _)| *s == side && *sl == slot) {
                        Some((_, _, items)) => items.push((kind.id(), item)),
                        None => out.bars.push((side, slot, vec![(kind.id(), item)])),
                    }
                }
                ModuleMode::Off => {}
            }
        }
        out
    }

    /// 点了胶囊右侧的闪烁图标：转发左键给真实托盘图标（与点 Dock 上的托盘一个效果）。
    /// 没有回调目标（UIA 兜底还没扫到）就忽略——Dock 那边的图标仍是入口。
    fn click_flash(&mut self, i: usize) {
        let Some(f) = self.flash.get(i) else { return };
        let Some(raw) = f.target else { return };
        let t = traymsg::Target::from_raw(raw);
        let now = std::time::Instant::now();
        let dbl = self.last_lclick.map_or(false, |lt| {
            now.duration_since(lt).as_millis() as u32
                <= unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() }
        });
        self.last_lclick = Some(now);
        // 坐标用胶囊里这个图标自己的中心（v4 编码进 wParam；应用常用它定位菜单）
        let (cx, cy) = self
            .widget_rects
            .get(self.widget_kinds.len() + i)
            .map(|r| {
                (
                    self.pos.0 + (r[0] + r[2] / 2.0) as i32,
                    self.pos.1 + (r[1] + r[3] / 2.0) as i32,
                )
            })
            .unwrap_or(self.pos);
        crate::tlog!("[pill] 点击闪烁图标 {}（{}）", f.name, f.key);
        traymsg::click(&t, (cx, cy), dbl);
    }

    /// 闪烁图标**悬停进入**：与 Dock 托盘转发同一套回调消息（hover 事件），
    /// 并启动 flyout 守位线程把应用自己弹的预览卡片摆到**胶囊下方**——
    /// 胶囊贴着屏幕顶，卡片放上方会直接出屏。
    fn flash_hover_enter(&mut self, i: usize) {
        self.flash_hover_sent = Some(i);
        let Some(f) = self.flash.get(i) else { return };
        let Some(raw) = f.target else { return };
        let t = traymsg::Target::from_raw(raw);
        let Some(r) = self.widget_rects.get(self.widget_kinds.len() + i) else { return };
        let cx = self.pos.0 + (r[0] + r[2] / 2.0) as i32;
        let cy = self.pos.1 + (r[1] + r[3] / 2.0) as i32;
        crate::tlog!("[pill] 悬停闪烁图标 {}（{}）", f.name, f.key);
        traymsg::hover(&t, (cx, cy));
        let cap = RECT {
            left: self.pos.0,
            top: self.pos.1,
            right: self.pos.0 + self.pill.0,
            bottom: self.pos.1 + self.pill.1,
        };
        crate::flyout::watch(t.pid, t.hwnd, cap, cx, &f.name, true);
    }

    /// 闪烁图标悬停离开：补发 leave（Dock 同款）
    fn flash_hover_leave(&mut self) {
        let Some(i) = self.flash_hover_sent.take() else { return };
        if let Some(f) = self.flash.get(i) {
            if let Some(raw) = f.target {
                let t = traymsg::Target::from_raw(raw);
                traymsg::leave(&t, (0, 0));
            }
        }
    }

    // ---------- widget 点击 / 面板 ----------

    fn on_widget(&mut self, kind: WidgetKind) {
        match kind {
            WidgetKind::Mail => {
                // 正在显示验证码：点击 = 复制并收回，不展开面板
                if let Badge::Code(code) = self.mail.badge() {
                    clipboard::set_text(&code);
                    self.mail.consume_code();
                    self.render();
                    return;
                }
                self.toggle_panel(kind);
            }
            WidgetKind::Weather | WidgetKind::Clipboard => self.toggle_panel(kind),
        }
    }

    fn toggle_panel(&mut self, kind: WidgetKind) {
        if self.panel == Some(kind) {
            self.close_panel();
            return;
        }
        let content = self.panel_content(kind);
        let ax = self.panel_anchor_x(kind);
        let ty = self.panel_top_y(&content);
        panel::show(self.hwnd, self.scale, content, ax, ty);
        self.panel = Some(kind);
        self.panel_tab = 0;
        self.panel_outside = 0;
    }

    /// 用最新数据重绘当前面板（保持展开状态）
    fn reload_panel(&mut self) {
        let Some(kind) = self.panel else { return };
        let content = self.panel_content(kind);
        let ax = self.panel_anchor_x(kind);
        let ty = self.panel_top_y(&content);
        panel::show(self.hwnd, self.scale, content, ax, ty);
        self.panel_outside = 0;
    }

    /// 面板的水平锚点：列表面板锚在对应 widget 的中心；
    /// 天气卡片比胶囊宽得多，直接和胶囊中心对齐（不跟着天气 widget 跑）。
    /// widget 被分离到左右条时，锚点跟着到条上那个 widget 的中心。
    fn panel_anchor_x(&self, kind: WidgetKind) -> i32 {
        match kind {
            WidgetKind::Weather => self.pos.0 + self.pill.0 / 2,
            _ => self
                .widget_kinds
                .iter()
                .position(|k| *k == kind)
                .and_then(|i| self.widget_rects.get(i))
                .map(|r| self.pos.0 + (r[0] + r[2] / 2.0) as i32)
                .or_else(|| widgetbar::widget_center(kind.id()))
                .unwrap_or(self.pos.0 + self.pill.0 / 2),
        }
    }

    /// 面板的纵向位置：胶囊悬浮/吸顶时弹在胶囊下方；胶囊吸底时弹在**上方**
    ///（不然面板会被挤出屏幕）。
    fn panel_top_y(&self, content: &Content) -> i32 {
        if settings::pill_anchor() == PillAnchor::Bottom {
            let (_, ph) = panel::content_size(content, self.scale);
            (self.pos.1 - cfg::PANEL_GAP - ph).max(primary_monitor().rcMonitor.top + 4)
        } else {
            self.pos.1 + self.pill.1 + cfg::PANEL_GAP
        }
    }

    fn close_panel(&mut self) {
        panel::hide();
        self.panel = None;
        self.panel_tab = 0;
    }

    /// 鼠标离开胶囊与面板约 1 秒后自动收起（500ms 一次的定时器驱动）
    fn tick_panel(&mut self) {
        if self.panel.is_none() {
            return;
        }
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let mut wr = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut wr);
        }
        let in_pill = pt.x >= wr.left && pt.x <= wr.right && pt.y >= wr.top && pt.y <= wr.bottom;
        // 分离出去的模块条（点它开的 panel）也算"内部"，否则鼠标停在条上就被收起了
        if in_pill || panel::contains(pt) || widgetbar::contains(pt) {
            self.panel_outside = 0;
            return;
        }
        self.panel_outside += 1;
        if self.panel_outside >= 2 {
            self.close_panel();
        }
    }

    /// 驱动胶囊旁的音乐条（封面 + 歌词）：换歌/取词/播放进度变化时才动窗口。
    /// 没有歌曲播放时整条隐藏——原来的音乐 widget 就是这么并进来的。
    fn tick_bar(&mut self) {
        if !cfg::SHOW_MUSIC {
            musicbar::hide();
            self.set_anim(false);
            return;
        }
        self.music.tick();
        self.lyrics.tick(self.music.now.as_ref());
        if self.music.now.is_none() {
            musicbar::hide();
            self.cover = None;
            self.angle = 0.0;
            self.last_track = (String::new(), String::new());
            self.set_anim(false);
            return;
        }

        // 换曲：角度归零（封面版本号由 Music 递增，解码在下面按版本做一次）
        let playing = self.music.playing();
        let track_changed = match self.music.now.as_ref() {
            Some(n) => n.title != self.last_track.0 || n.artist != self.last_track.1,
            None => false,
        };
        if track_changed {
            if let Some(n) = self.music.now.as_ref() {
                self.last_track = (n.title.clone(), n.artist.clone());
            }
            self.angle = 0.0;
            self.angle_at = std::time::Instant::now();
        }

        // 封面：每首歌只解码一次（失败就记住这个版本没有封面，不反复试）
        // 只按 art_ver 缓存：一次解码就按"胶囊放到最大"的像素数解，
        // 之后无论用户把胶囊调大调小，封面都不会因为重采样而发虚
        if self.cover.as_ref().map(|c| c.0) != Some(self.music.art_ver) {
            let px = render::cover_px(self.scale * settings::PILL_SCALE_MAX);
            let rgba = self
                .music
                .art
                .as_deref()
                .and_then(|b| render::decode_cover(b, px))
                .unwrap_or_default();
            self.cover = Some((self.music.art_ver, rgba));
        }

        // 角度只在播放时推进（暂停时定格）
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.angle_at).as_secs_f32();
        self.angle_at = now;
        if playing {
            self.angle = (self.angle + dt * cfg::COVER_DEG_PER_SEC) % 360.0;
        }

        let pos = self.music.position_ms().max(0) as u64;
        let text = match self.lyrics.line_text(pos) {
            // 有词就显示当前这句；前奏（第一句之前）与整首无词时退回歌名
            Some(t) => t.to_string(),
            None => self.music.title_line().unwrap_or_default(),
        };
        let cover = self
            .cover
            .as_ref()
            .map(|(_, px)| px.as_slice())
            .filter(|s| !s.is_empty());

        {
            let lyric_pos = settings::lyric_position();
            let view = musicbar::View {
                text: &text,
                cover,
                cover_id: self.music.art_ver,
                angle: self.angle,
                playing,
                scale: self.scale,
                font: settings::lyric_size(),
                // 该侧如果已经贴了分离出来的模块条，音乐条就让到它的外面去
                pill: match lyric_pos {
                    LyricPos::Left => self.side_anchor(BarSide::Left),
                    LyricPos::Right => self.side_anchor(BarSide::Right),
                    LyricPos::Bottom => (self.pos.0, self.pos.1, self.pill.0, self.pill.1),
                },
                pos: lyric_pos,
            };
            musicbar::show(&view);
        }
        self.set_anim(playing);
    }

    /// 某一侧的锚矩形：该侧有分离模块条就用条的矩形，否则用胶囊本身
    fn side_anchor(&self, side: BarSide) -> (i32, i32, i32, i32) {
        widgetbar::rect(side).unwrap_or((self.pos.0, self.pos.1, self.pill.0, self.pill.1))
    }

    /// 封面旋转定时器：只在播放时开着，省掉暂停时的空转
    fn set_anim(&mut self, on: bool) {
        if on == self.anim_on || self.hwnd.is_invalid() {
            return;
        }
        self.anim_on = on;
        unsafe {
            if on {
                SetTimer(self.hwnd, T_COVER, 40, None);
            } else {
                let _ = KillTimer(self.hwnd, T_COVER);
            }
        }
    }

    /// 面板动作号分发：<1000 行 / 1000+ 固定 / 2000+ 标签页
    fn on_panel_click(&mut self, id: u32) {
        match self.panel {
            Some(WidgetKind::Weather) => {
                // 天气卡片只有"刷新"一个热点：立即重取并重绘面板
                if id == 0 {
                    self.weather.refresh();
                    self.weather.tick();
                    self.reload_panel();
                }
            }
            Some(WidgetKind::Mail) => {
                if id >= TAB_BASE {
                    self.panel_tab = ((id - TAB_BASE) as usize).min(1);
                    self.reload_panel();
                } else if id < PIN_BASE {
                    let tab = self.panel_tab;
                    let Some(&i) = self.mail.visible(tab).get(id as usize) else { return };
                    let uid = self.mail.items[i].uid.clone();
                    crate::tlog!("[mail] 点开邮件 #{id} items[{i}] uid={uid} body={}字", self.mail.items[i].body.chars().count());
                    self.mail.mark_read(&uid);
                    self.render();
                    self.reload_panel();
                    // 打开阅读窗：正文可选中复制；窗内可删除。
                    // 位置贴在邮件面板右侧（面板被遮住时收进屏幕）。
                    if let Some(m) = self.mail.items.iter().find(|m| m.uid == uid) {
                        let (px, py, pw, _) =
                            panel::rect().unwrap_or((self.pos.0 + self.pill.0 / 2, self.pos.1 + self.pill.1, 0, 0));
                        mailview::show(self.hwnd, self.scale, &m.subject, &m.from, &m.body, &m.uid, px + pw + 8, py);
                    }
                }
            }
            Some(WidgetKind::Clipboard) => {
                if id >= PIN_BASE {
                    self.clipboard.toggle_pin((id - PIN_BASE) as usize);
                    self.reload_panel();
                } else if let Some(text) = self.clipboard.items.get(id as usize).map(|c| c.text.clone()) {
                    clipboard::set_text(&text);
                    self.close_panel();
                }
            }
            _ => {}
        }
    }

    /// 按 widget 类型生成面板内容
    fn panel_content(&self, kind: WidgetKind) -> Content {
        let mut rows: Vec<PanelRow> = Vec::new();

        match kind {
            WidgetKind::Weather => {
                let Some(s) = &self.weather.snap else {
                    return Content::List {
                        title: "当地天气".into(),
                        subtitle: self.weather.error.clone().unwrap_or_else(|| "正在获取…".into()),
                        tabs: vec![],
                        tab_active: 0,
                        rows,
                        empty: "暂无数据".into(),
                    };
                };
                let (date, time) = now_labels();
                let mut parts: Vec<String> = Vec::new();
                if let Some(f) = s.feels {
                    parts.push(format!("体感 {f}°"));
                }
                parts.push(format!("湿度 {}%", s.humidity));
                if !s.wind_dir.is_empty() || !s.wind_power.is_empty() {
                    parts.push(format!("{}{}级", s.wind_dir, s.wind_power));
                } else if s.wind > 0 {
                    parts.push(format!("风 {} km/h", s.wind));
                }
                let city = if s.place.is_empty() { s.province.clone() } else { s.place.clone() };
                let report = if s.report.is_empty() {
                    format!("数据源 {}", s.source)
                } else {
                    format!("{} · {} 发布", s.source, s.report)
                };
                // 一行最多两项，避免字号调大后横向溢出
                let details: Vec<String> = parts.chunks(2).map(|c| c.join(" · ")).collect();
                let view = render::WeatherView {
                    city,
                    temp: format!("{}°", s.temp),
                    condition: s.condition.clone(),
                    icon: weather::glyph(s.code, s.day).to_string(),
                    icon_argb: weather_color(s.code, s.day),
                    details,
                    report,
                    date,
                    time,
                    days: s
                        .days
                        .iter()
                        .take(4)
                        .map(|d| {
                            let text = if d.day_weather == d.night_weather {
                                d.day_weather.clone()
                            } else {
                                format!("{} / {}", d.day_weather, d.night_weather)
                            };
                            let (note, note_argb) = if !d.day_power.is_empty() {
                                (format!("{}{}级", d.day_wind, d.day_power), cfg::cur().icon_dim)
                            } else if d.rain > 0 {
                                (format!("{}%", d.rain), cfg::cur().accent)
                            } else {
                                (String::new(), cfg::cur().icon_dim)
                            };
                            render::WeatherDay {
                                label: d.label.clone(),
                                icon: weather::glyph(d.code, true).to_string(),
                                icon_argb: weather_color(d.code, true),
                                high: format!("{}°", d.high),
                                low: format!("{}°", d.low),
                                text,
                                note,
                                note_argb,
                            }
                        })
                        .collect(),
                };
                Content::Weather(Box::new(view))
            }
            WidgetKind::Clipboard => {
                // 固定的排在最前（Clipboard 内部已保证顺序），行号 = items 下标
                for c in self.clipboard.items.iter().take(12) {
                    rows.push(PanelRow {
                        icon: String::new(),
                        icon_argb: 0,
                        title: clipboard::preview(&c.text, 40),
                        sub: String::new(),
                        value: c.at.clone(),
                        value_argb: cfg::cur().text_dim,
                        pin: Some(c.pinned),
                    });
                }
                let n = self.clipboard.items.len();
                let pinned = self.clipboard.items.iter().filter(|c| c.pinned).count();
                let subtitle = if n == 0 {
                    "暂无记录".into()
                } else if pinned > 0 {
                    format!("{n} 条 · {pinned} 条固定 · 点击复制")
                } else {
                    format!("{n} 条 · 点击复制 · ☆ 固定")
                };
                Content::List {
                    title: "剪贴板".into(),
                    subtitle,
                    tabs: vec![],
                    tab_active: 0,
                    rows,
                    empty: "还没有记录".into(),
                }
            }
            WidgetKind::Mail => {
                let tab = self.panel_tab.min(1);
                for i in self.mail.visible(tab) {
                    let m = &self.mail.items[i];
                    rows.push(PanelRow {
                        // 未读用强调色，已读压暗；点开即已读
                        icon: "✉".into(),
                        icon_argb: if m.read { cfg::cur().icon_dim } else { cfg::cur().accent },
                        title: if m.subject.is_empty() { "(无主题)".into() } else { m.subject.clone() },
                        sub: m.from.clone(),
                        value: m.code.clone().unwrap_or_default(),
                        value_argb: cfg::cur().sun,
                        pin: None,
                    });
                }
                let subtitle = match &self.mail.error {
                    Some(e) => format!("{} · {e}", cfg::MAIL_USER),
                    None => format!("{} · {} 封未读 · 点标题阅读", cfg::MAIL_USER, self.mail.unread()),
                };
                Content::List {
                    title: "邮箱".into(),
                    subtitle,
                    tabs: vec!["未读".into(), "全部".into()],
                    tab_active: tab,
                    rows,
                    empty: if tab == 0 { "没有未读邮件".into() } else { "没有邮件".into() },
                }
            }
        }
    }
}

fn weather_color(code: u8, day: bool) -> u32 {
    match code {
        0 | 1 => {
            if day {
                cfg::cur().sun
            } else {
                cfg::cur().moon
            }
        }
        2 | 3 | 45 | 48 => cfg::cur().cloud,
        51..=67 | 80..=99 => cfg::cur().rain,
        _ => cfg::cur().icon_dim,
    }
}

fn now_parts() -> (u32, u16, u16) {
    let st = unsafe { GetLocalTime() };
    // 只有"带秒"的格式才把秒算进变化比较，否则每秒都会白重绘一次
    let sec = if clock::shows_seconds() { st.wSecond } else { 0 };
    (st.wHour as u32 * 60 + st.wMinute as u32, st.wDay, sec)
}

/// 面板标题栏用的日期与时间文本
fn now_labels() -> (String, String) {
    let st = unsafe { GetLocalTime() };
    let time = format!("{:02}:{:02}", st.wHour, st.wMinute);
    let wd = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"][(st.wDayOfWeek % 7) as usize];
    (format!("{}月{}日 {}", st.wMonth, st.wDay, wd), time)
}

pub fn primary_monitor() -> MONITORINFO {
    let mut mi = MONITORINFO::default();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    unsafe {
        let hmon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let _ = GetMonitorInfoW(hmon, &mut mi);
    }
    mi
}
