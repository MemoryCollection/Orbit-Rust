//! 顶部胶囊窗口：置顶分层窗口、命中测试（圆角外穿透）、悬停、时钟/widget 轮询，
//! 以及展开面板的开关与点击分发。托盘图标不在这里（在底部 Dock 右侧）。

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, MOD_ALT, MOD_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetCursorPos, GetWindowLongPtrW, GetWindowRect, KillTimer, PostQuitMessage,
    SetTimer, SetWindowLongPtrW, SetWindowPos, CREATESTRUCTW,
    GWLP_USERDATA, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, SWP_NOACTIVATE, WM_CREATE, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_NCHITTEST, WM_TIMER,
};

use crate::clipboard::{self, Clipboard};
use crate::config as cfg;
use crate::hide;
use crate::lyric::Lyrics;
use crate::mail::{Badge, Mail};
use crate::music::{Cmd as MusicCmd, Music};
use crate::musicbar;
use crate::panel::{self, Content, WM_ROW_CLICK};
use crate::render::{self, FontKind, PanelRow, WidgetItem, PIN_BASE, TAB_BASE};
use crate::settings;
use crate::weather::{self, Weather};

/// 定时器 ID（SetTimer 的 usize 标识）
const T_CLOCK: usize = 1;
/// 原生任务栏隐藏状态的补丁轮询（explorer 重启/换主题会抹掉分层样式）
const T_HIDE: usize = 2;
/// 歌词对齐用的快定时器（200ms）
const T_LYRIC: usize = 4;
/// 封面旋转用的帧定时器（40ms，只在播放时开着）
const T_COVER: usize = 5;

pub struct App {
    hwnd: HWND,
    scale: f32,
    /// 上次绘制的 (时间:分钟, 日)，变化才重绘
    last_time: (u32, u16),
    hovered_widget: Option<usize>,
    mouse_tracked: bool,
    widget_rects: Vec<[f32; 4]>,
    widget_kinds: Vec<WidgetKind>,
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
}

/// 胶囊里的 widget 类型（决定点击行为与面板内容）
#[derive(Clone, Copy, PartialEq, Debug)]
enum WidgetKind {
    Weather,
    Clipboard,
    Mail,
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
                    // settings.txt 被改过（设置页面/手改）就热更新
                    if settings::reload_if_changed() {
                        dirty = true;
                        if app.panel.is_some() {
                            app.reload_panel();
                        }
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
                }
                LRESULT(0)
            }
            WM_NCHITTEST => {
                // lParam 是屏幕坐标，转 client 后做胶囊形命中
                let mut pt = POINT {
                    x: (l.0 & 0xFFFF) as u16 as i16 as i32,
                    y: ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
                };
                let mut wr = RECT::default();
                let _ = GetWindowRect(hwnd, &mut wr);
                pt.x -= wr.left;
                pt.y -= wr.top;
                if app.widget_index_at(pt.x as f32, pt.y as f32).is_some()
                    || inside_pill(pt.x as f32, pt.y as f32, app.pill.0, app.pill.1)
                {
                    LRESULT(HTCLIENT as isize)
                } else {
                    LRESULT(HTTRANSPARENT as isize)
                }
            }
            WM_MOUSEMOVE => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                let widx = app.widget_index_at(x as f32, y as f32);
                if widx != app.hovered_widget {
                    app.hovered_widget = widx;
                    app.render();
                }
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
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                app.mouse_tracked = false;
                if app.hovered_widget.take().is_some() {
                    app.render();
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                if let Some(wi) = app.widget_index_at(x as f32, y as f32) {
                    if let Some(kind) = app.widget_kinds.get(wi).copied() {
                        app.on_widget(kind);
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => LRESULT(0),
            WM_DISPLAYCHANGE => {
                app.render();
                LRESULT(0)
            }
            WM_HOTKEY if w.0 == 1 => {
                // Ctrl+Alt+O：退出并恢复原生任务栏
                PostQuitMessage(0);
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
            // 音乐条的封面按钮：左键播放/暂停，右键下一首
            m if m == musicbar::WM_TOGGLE => {
                app.music.send(MusicCmd::Toggle);
                LRESULT(0)
            }
            m if m == musicbar::WM_NEXT => {
                app.music.send(MusicCmd::Next);
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
            last_time: (0, 0),
            hovered_widget: None,
            mouse_tracked: false,
            widget_rects: vec![],
            widget_kinds: vec![],
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
        }
    }

    pub fn init(&mut self, hwnd: HWND) {
        settings::init();
        unsafe {
            self.hwnd = hwnd;
            self.scale = GetDpiForWindow(hwnd) as f32 / 96.0;
            self.last_time = now_parts();
            // 退出热键：Ctrl+Alt+O
            let _ = RegisterHotKey(hwnd, 1, MOD_CONTROL | MOD_ALT, 'O' as u32);
        }
    }

    /// 计算布局 → GDI+ 绘制 → UpdateLayeredWindow → 置顶定位
    pub fn render(&mut self) {
        let scale = self.scale;
        let (time, date) = now_strings();
        let (tw, _) = render::measure_text(&time, cfg::TIME_SIZE, FontKind::Latin, scale);
        let (dw, _) = render::measure_text(&date, cfg::DATE_SIZE, FontKind::Cjk, scale);
        let (widgets, kinds) = self.build_widgets();
        let w = render::content_width(tw, dw, &widgets, scale).ceil() as i32;
        let h = (cfg::PILL_H * scale).ceil() as i32;
        self.pill = (w, h);

        let out = render::draw_pill(w, h, scale, &time, &date, &widgets, self.hovered_widget);
        self.widget_rects = out.widget_rects;
        self.widget_kinds = kinds;
        let bgra = render::to_premultiplied_bgra(&out.rgba);
        render::present(self.hwnd, &bgra, w, h);

        // 定位：主屏顶部居中（固定位置，不可拖动）
        let mx = primary_monitor().rcMonitor;
        let x = mx.left + (mx.right - mx.left - w) / 2;
        let y = mx.top + cfg::TOP_MARGIN;
        self.pos = (x, y);
        unsafe {
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
        }
        // 胶囊宽度/位置变了（换了 widget、改了字号），音乐条跟着重新贴一次
        self.tick_bar();
    }

    /// 组装当前可见的 widget（无数据的不占位）
    fn build_widgets(&self) -> (Vec<WidgetItem>, Vec<WidgetKind>) {
        let mut items = Vec::new();
        let mut kinds = Vec::new();
        if cfg::SHOW_WEATHER {
            if let Some(s) = &self.weather.snap {
                items.push(WidgetItem {
                    icon: weather::glyph(s.code, s.day).to_string(),
                    icon_argb: weather_color(s.code, s.day),
                    text: format!("{}°", s.temp),
                });
                kinds.push(WidgetKind::Weather);
            }
        }
        if cfg::SHOW_MAIL && (!self.mail.items.is_empty() || self.mail.error.is_some()) {
            // 平时只有图标；识别到验证码显示验证码；有未读但没有验证码就提示有新邮件
            let (text, argb) = match self.mail.badge() {
                Badge::Code(code) => (code, cfg::SUN_ARGB),
                Badge::Unread => ("新邮件".to_string(), cfg::ACCENT_ARGB),
                Badge::None => (String::new(), cfg::TEXT_DIM_ARGB),
            };
            items.push(WidgetItem { icon: "✉".to_string(), icon_argb: argb, text });
            kinds.push(WidgetKind::Mail);
        }
        if cfg::SHOW_CLIPBOARD && !self.clipboard.items.is_empty() {
            // 只显示图标，不暴露当前剪贴板内容
            items.push(WidgetItem {
                icon: "▤".to_string(),
                icon_argb: cfg::TEXT_DIM_ARGB,
                text: String::new(),
            });
            kinds.push(WidgetKind::Clipboard);
        }
        (items, kinds)
    }

    fn widget_index_at(&self, x: f32, y: f32) -> Option<usize> {
        self.widget_rects.iter().position(|r| {
            x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3]
        })
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
        let (ax, ty) = self.panel_anchor(kind);
        panel::show(self.hwnd, self.scale, content, ax, ty);
        self.panel = Some(kind);
        self.panel_tab = 0;
        self.panel_outside = 0;
    }

    /// 用最新数据重绘当前面板（保持展开状态）
    fn reload_panel(&mut self) {
        let Some(kind) = self.panel else { return };
        let content = self.panel_content(kind);
        let (ax, ty) = self.panel_anchor(kind);
        panel::show(self.hwnd, self.scale, content, ax, ty);
        self.panel_outside = 0;
    }

    /// 面板的 (水平锚点, 顶部 y)：锚在对应 widget 的中心，下方留 6px
    fn panel_anchor(&self, kind: WidgetKind) -> (i32, i32) {
        let x = self
            .widget_kinds
            .iter()
            .position(|k| *k == kind)
            .and_then(|i| self.widget_rects.get(i))
            .map(|r| self.pos.0 + (r[0] + r[2] / 2.0) as i32)
            .unwrap_or(self.pos.0 + self.pill.0 / 2);
        (x, self.pos.1 + self.pill.1 + cfg::PANEL_GAP)
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
        if in_pill || panel::contains(pt) {
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
        if self.cover.as_ref().map(|c| c.0) != Some(self.music.art_ver) {
            let px = render::cover_px(self.scale);
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
            let view = musicbar::View {
                text: &text,
                cover,
                cover_id: self.music.art_ver,
                angle: self.angle,
                playing,
                scale: self.scale,
                font: settings::lyric_size(),
                pill: (self.pos.0, self.pos.1, self.pill.0, self.pill.1),
                pos: settings::lyric_position(),
            };
            musicbar::show(&view);
        }
        self.set_anim(playing);
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
                    self.mail.mark_read(&uid);
                    self.render();
                    self.reload_panel();
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
                                (format!("{}{}级", d.day_wind, d.day_power), cfg::TEXT_DIM_ARGB)
                            } else if d.rain > 0 {
                                (format!("{}%", d.rain), cfg::ACCENT_ARGB)
                            } else {
                                (String::new(), cfg::TEXT_DIM_ARGB)
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
                        value_argb: cfg::DATE_ARGB,
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
                        icon_argb: if m.read { cfg::TEXT_DIM_ARGB } else { cfg::ACCENT_ARGB },
                        title: if m.subject.is_empty() { "(无主题)".into() } else { m.subject.clone() },
                        sub: m.from.clone(),
                        value: m.code.clone().unwrap_or_default(),
                        value_argb: cfg::SUN_ARGB,
                        pin: None,
                    });
                }
                let subtitle = match &self.mail.error {
                    Some(e) => format!("{} · {e}", cfg::MAIL_USER),
                    None => format!("{} · {} 封未读", cfg::MAIL_USER, self.mail.unread()),
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
                cfg::SUN_ARGB
            } else {
                cfg::MOON_ARGB
            }
        }
        2 | 3 | 45 | 48 => cfg::CLOUD_ARGB,
        51..=67 | 80..=99 => cfg::RAIN_ARGB,
        _ => cfg::TEXT_DIM_ARGB,
    }
}

/// 圆角胶囊形命中：四角圆弧外穿透
fn inside_pill(x: f32, y: f32, w: i32, h: i32) -> bool {
    let (fw, fh) = (w as f32, h as f32);
    if x < 0.0 || x > fw || y < 0.0 || y > fh {
        return false;
    }
    let r = fh / 2.0;
    if x >= r && x <= fw - r {
        return true;
    }
    let cx = if x < r { r } else { fw - r };
    let dx = (x - cx).abs();
    let dy = (y - r).abs();
    dx * dx + dy * dy <= r * r
}

fn now_parts() -> (u32, u16) {
    let st = unsafe { GetLocalTime() };
    (st.wHour as u32 * 60 + st.wMinute as u32, st.wDay)
}

/// 面板标题栏用的日期与时间文本
fn now_labels() -> (String, String) {
    let st = unsafe { GetLocalTime() };
    let time = format!("{:02}:{:02}", st.wHour, st.wMinute);
    let wd = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"][(st.wDayOfWeek % 7) as usize];
    (format!("{}月{}日 {}", st.wMonth, st.wDay, wd), time)
}

fn now_strings() -> (Vec<u16>, Vec<u16>) {    let st = unsafe { GetLocalTime() };
    let time: Vec<u16> = format!("{:02}:{:02}", st.wHour, st.wMinute).encode_utf16().collect();
    let wd = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"][(st.wDayOfWeek % 7) as usize];
    let date: Vec<u16> = format!("{}月{}日 {}", st.wMonth, st.wDay, wd).encode_utf16().collect();
    (time, date)
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
