//! 可调设置：`<exe>/data/settings.txt`，一行一个 `key=value`。
//! 启动时读一次；运行时每 500ms 查一次文件是否被改动，改了就热更新并重绘，
//! 所以后续设置页面只需要写这个文件（或调用这里的 set_*）。
//!
//! 当前键：
//! - `clock_format`       时钟格式串（strftime 风格）：`%Y-%m-%d %H:%M:%S`、
//!                          `%H:%M %m月%d日 %a`（默认）。占位符见 `clock.rs` 头注释。
//!                          老版本的 `clock_order` / `time_format` / `date_format`
//!                          仍会被读取并自动迁移成这条格式串。
//! - `module_order`       顶部胶囊模块的排列（拖动排序写回）：`weather,mail,clipboard`
//! - `weather_mode` / `mail_mode` / `clipboard_mode`
//!   模块摆哪：`pill`（留在胶囊里）/ `left`（分离到胶囊左侧一条）/ `right` / `off`（隐藏）
//! - `pill_alpha`         胶囊（含分离模块条）整体透明度 0.2 ~ 1.0，默认 1.0。
//!                         拉到 100% 时底色**强制实底**（半透明主题也变不透明），
//!                         见 `config::pill_bg_effective`
//! - `pill_scale`         胶囊（含模块条 / 音乐条）整体大小 0.8 ~ 1.35，默认 1.0
//! - `icon_scale`         Dock 图标（连同 Dock 高度与托盘图标）大小 0.8 ~ 1.35，默认 1.0
//! - `theme`              外观主题 key（`calm_light` / `calm_dark` / `glass2_light` /
//!                         `glass2_dark` / `m3_light` / `m3_dark`）；三套风格各带深浅色。
//!                         老版本的 `glass`/`liquid`/`mica`/`paper`/`neon`/`terminal`
//!                         会被 `legacy_theme_key` 就近映射到新 key
//! - `pill_anchor`        胶囊位置/形状：`float`（悬浮顶部，默认）/ `island`（吸顶刘海，
//!                          顶部两角方形）/ `bottom`（吸底，底部两角方形）
//! - `dock_anchor`        底部 Dock 位置：`bottom`（默认）/ `top` / `left` / `right`
//! - `weather_source`     天气数据源：`amap`（高德，默认）/ `open_meteo`
//! - `amap_key`           高德 Web 服务 key（空 = 回落编译期默认）
//! - `imap_host` / `imap_port` / `imap_user` / `imap_pass`
//!   邮箱 IMAP 账号（设置页可改；空值回落 `data/mail.txt`，再回落编译期默认）
//! - `weather_text_scale` 天气面板字号倍率（1.0 = 基准，0.8 ~ 1.8）
//! - `lyric_position`     音乐条位置：`right`（默认，贴胶囊右侧）/ `left` / `bottom`
//! - `lyric_size`         歌词字号（逻辑单位，10 ~ 26，默认 14）
//! - `show_tray`          Dock 上是否显示托盘图标（0 = 隐藏；仅手改文件生效）
//! - `show_desktop_btn`   Dock"返回桌面"细长条（0 = 不显示；设置页在「托盘图标」
//!                          列表的"返回桌面"格里开关）
//! - `hide_tray`          Dock 上要**单独隐藏**的托盘图标（分号分隔，键由程序生成）
//!
//! 设置页面（`settingsui.rs`）改值走的也是这里：`set_*` 更新内存 + 重写文件，
//! 并把 `VERSION` 加一；各窗口在自己的定时器里比对版本号决定是否重绘。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::UNIX_EPOCH;

use crate::config as cfg;
use crate::store;

/// 天气字号倍率上下限（再小看不清，再大卡片会被挤出工作区）
const SCALE_MIN: f32 = 0.8;
const SCALE_MAX: f32 = 1.8;
/// 歌词字号上下限
const LYRIC_MIN: f32 = 10.0;
const LYRIC_MAX: f32 = 26.0;
/// 胶囊透明度上下限（再低就看不清了）；滑条量程直接用这两个值
pub const ALPHA_MIN: f32 = 0.2;
pub const ALPHA_MAX: f32 = 1.0;
/// 胶囊（含模块条 / 音乐条）整体缩放上下限。1.0 = 基准尺寸。
/// 缩放的是"逻辑点"而不是像素：文字与图标按新尺寸重新排版光栅化，不会发虚。
pub const PILL_SCALE_MIN: f32 = 0.80;
pub const PILL_SCALE_MAX: f32 = 1.35;
/// Dock 图标缩放上下限（Dock 高度 / 槽位 / 托盘图标随之等比变化）
pub const ICON_SCALE_MIN: f32 = 0.80;
pub const ICON_SCALE_MAX: f32 = 1.35;
/// AtomicU32 的 0 表示"还没加载"
const UNLOADED: u32 = 0;

static SCALE: AtomicU32 = AtomicU32::new(UNLOADED);
static LYRIC_SIZE: AtomicU32 = AtomicU32::new(UNLOADED);
/// 歌词位置编码：1 = 左，2 = 右，3 = 下（0 = 未加载）
static LYRIC_POS: AtomicU32 = AtomicU32::new(UNLOADED);
/// 模块排列（列表）与各模块的模式
static MODULE_ORDER: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| {
    Mutex::new(vec![
        "weather".to_string(),
        "mail".to_string(),
        "clipboard".to_string(),
    ])
});
static WEATHER_MODE: AtomicU32 = AtomicU32::new(UNLOADED);
static MAIL_MODE: AtomicU32 = AtomicU32::new(UNLOADED);
static CLIPBOARD_MODE: AtomicU32 = AtomicU32::new(UNLOADED);
/// 时钟格式串
static CLOCK_FORMAT: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));
/// 胶囊透明度（f32 位型）/ 锚位编码 / Dock 锚位编码
static PILL_ALPHA: AtomicU32 = AtomicU32::new(UNLOADED);
/// 胶囊缩放 / Dock 图标缩放（f32 位型）
static PILL_SCALE: AtomicU32 = AtomicU32::new(UNLOADED);
static ICON_SCALE: AtomicU32 = AtomicU32::new(UNLOADED);
static PILL_ANCHOR: AtomicU32 = AtomicU32::new(UNLOADED);
static DOCK_ANCHOR: AtomicU32 = AtomicU32::new(UNLOADED);
/// 天气数据源编码 / 高德 key
static WEATHER_SOURCE: AtomicU32 = AtomicU32::new(UNLOADED);
static AMAP_KEY: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));
/// 邮箱账号（空字符串 = 回落 mail.txt / 编译期默认）
static IMAP_HOST: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));
static IMAP_PORT: AtomicU32 = AtomicU32::new(UNLOADED);
static IMAP_USER: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));
static IMAP_PASS: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));
/// Dock 上是否显示托盘图标 / 是否显示"返回桌面"细长条（0 = 未加载，1 = 关，2 = 开）
static SHOW_TRAY: AtomicU32 = AtomicU32::new(UNLOADED);
static SHOW_DESKTOP: AtomicU32 = AtomicU32::new(UNLOADED);
/// 微信托盘图标开始闪烁（新消息）时是否播放内置提示音
static WECHAT_SOUND: AtomicU32 = AtomicU32::new(UNLOADED);
/// 被单独隐藏的托盘图标键（`tray::TrayItem::key`）。数量很少，读写都走这把锁
static HIDE_TRAY: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// 设置版本号：任何值变化（手改文件或设置页面改）都 +1，各窗口据此判断要不要重绘
static VERSION: AtomicU64 = AtomicU64::new(1);
/// 正在写盘：这时读文件可能拿到半成品，轮询先跳过
static SAVING: AtomicU64 = AtomicU64::new(0);
static MTIME: AtomicU64 = AtomicU64::new(0);

// ---------- 模块模型 ----------

/// 模块摆哪：胶囊里 / 分离到胶囊左侧一条 / 右侧一条 / 隐藏。
/// 同侧多条按 `module_order` 的排列从胶囊往外排（**不再有 左1/左2/左3 编号**）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModuleMode {
    Pill,
    BarLeft,
    BarRight,
    Off,
}

impl ModuleMode {
    fn code(self) -> u32 {
        match self {
            ModuleMode::Pill => 1,
            ModuleMode::BarLeft => 2,
            ModuleMode::BarRight => 3,
            ModuleMode::Off => 4,
        }
    }

    fn from_code(c: u32) -> Self {
        match c {
            2 => ModuleMode::BarLeft,
            3 => ModuleMode::BarRight,
            4 => ModuleMode::Off,
            _ => ModuleMode::Pill,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "pill" | "capsule" | "in" | "胶囊" | "内" => Some(ModuleMode::Pill),
            "left" | "l" | "左" | "左侧" => Some(ModuleMode::BarLeft),
            "right" | "r" | "右" | "右侧" => Some(ModuleMode::BarRight),
            "off" | "hide" | "hidden" | "off_" | "隐藏" | "关" => Some(ModuleMode::Off),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ModuleMode::Pill => "pill",
            ModuleMode::BarLeft => "left",
            ModuleMode::BarRight => "right",
            ModuleMode::Off => "off",
        }
    }
}

/// 设置里认识的模块（名字即 settings.txt 里的键段，也是拖动排序的标识）
pub const MODULES: [&str; 3] = ["weather", "mail", "clipboard"];

fn mode_static(name: &str) -> (u32, u32) {
    // (加载后的值, 缺省值)
    match name {
        "weather" => (WEATHER_MODE.load(Ordering::Relaxed), ModuleMode::BarLeft.code()),
        "mail" => (MAIL_MODE.load(Ordering::Relaxed), ModuleMode::BarRight.code()),
        "clipboard" => (CLIPBOARD_MODE.load(Ordering::Relaxed), ModuleMode::BarRight.code()),
        _ => (UNLOADED, ModuleMode::Pill.code()),
    }
}

fn set_mode_static(name: &str, code: u32) {
    match name {
        "weather" => WEATHER_MODE.store(code, Ordering::Relaxed),
        "mail" => MAIL_MODE.store(code, Ordering::Relaxed),
        "clipboard" => CLIPBOARD_MODE.store(code, Ordering::Relaxed),
        _ => {}
    }
}

/// 模块排列（设置页拖动排序用；只包含认识的模块，去重、保序）
pub fn module_order() -> Vec<String> {
    MODULE_ORDER.lock().map(|v| v.clone()).unwrap_or_default()
}

pub fn set_module_order(order: Vec<String>) {
    let mut cleaned: Vec<String> = Vec::new();
    for name in order {
        let n = name.trim().to_lowercase();
        if MODULES.contains(&n.as_str()) && !cleaned.iter().any(|s| s == &n) {
            cleaned.push(n);
        }
    }
    // 漏掉的模块补到末尾（顺序仍完整）
    for m in MODULES {
        if !cleaned.iter().any(|s| s == m) {
            cleaned.push(m.to_string());
        }
    }
    if cleaned == module_order() {
        return;
    }
    if let Ok(mut v) = MODULE_ORDER.lock() {
        *v = cleaned;
    }
    save();
    bump();
}

pub fn module_mode(name: &str) -> ModuleMode {
    let (v, def) = mode_static(name);
    ModuleMode::from_code(if v == UNLOADED { def } else { v })
}

pub fn set_module_mode(name: &str, m: ModuleMode) {
    if module_mode(name) == m {
        return;
    }
    set_mode_static(name, m.code());
    save();
    bump();
}

// ---------- 胶囊 ----------

pub fn pill_alpha() -> f32 {
    match PILL_ALPHA.load(Ordering::Relaxed) {
        UNLOADED => 1.0,
        bits => f32::from_bits(bits),
    }
}

/// 拖动滑条时的**即时预览**：只改内存 + 通知重绘，不写盘。
///
/// 滑条每移一点就 `save()` 会不停重写 settings.txt（还要跟主线程的文件监视打架），
/// 所以拖动过程只走这里，松手时再 `persist_pill_alpha()` 落盘。
pub fn preview_pill_alpha(v: f32) {
    let v = v.clamp(ALPHA_MIN, ALPHA_MAX);
    if (v - pill_alpha()).abs() < 1e-4 {
        return;
    }
    PILL_ALPHA.store(v.to_bits(), Ordering::Relaxed);
    bump();
}

/// 把当前透明度落盘（滑条松手 / 键盘微调后调用）
pub fn persist_pill_alpha() {
    save();
    bump();
}

// ---------- 尺寸 ----------

/// 胶囊（顶部胶囊 + 分离模块条 + 音乐条）的整体缩放，1.0 = 基准
pub fn pill_scale() -> f32 {
    match PILL_SCALE.load(Ordering::Relaxed) {
        UNLOADED => 1.0,
        bits => f32::from_bits(bits).clamp(PILL_SCALE_MIN, PILL_SCALE_MAX),
    }
}

/// Dock 图标（连同 Dock 高度 / 槽位 / 托盘图标）的缩放，1.0 = 基准
pub fn icon_scale() -> f32 {
    match ICON_SCALE.load(Ordering::Relaxed) {
        UNLOADED => 1.0,
        bits => f32::from_bits(bits).clamp(ICON_SCALE_MIN, ICON_SCALE_MAX),
    }
}

/// 滑条拖动中的即时预览（只改内存 + 通知重绘，不写盘）
pub fn preview_pill_scale(v: f32) {
    let v = v.clamp(PILL_SCALE_MIN, PILL_SCALE_MAX);
    if (v - pill_scale()).abs() < 1e-4 {
        return;
    }
    PILL_SCALE.store(v.to_bits(), Ordering::Relaxed);
    bump();
}

pub fn persist_pill_scale() {
    save();
    bump();
}

pub fn preview_icon_scale(v: f32) {
    let v = v.clamp(ICON_SCALE_MIN, ICON_SCALE_MAX);
    if (v - icon_scale()).abs() < 1e-4 {
        return;
    }
    ICON_SCALE.store(v.to_bits(), Ordering::Relaxed);
    bump();
}

pub fn persist_icon_scale() {
    save();
    bump();
}

/// 一步到位地设值（「重置」按钮用）：改内存 + 落盘 + 通知重绘
pub fn set_pill_scale(v: f32) {
    let v = v.clamp(PILL_SCALE_MIN, PILL_SCALE_MAX);
    if (v - pill_scale()).abs() < 1e-4 {
        return;
    }
    PILL_SCALE.store(v.to_bits(), Ordering::Relaxed);
    save();
    bump();
}

pub fn set_icon_scale(v: f32) {
    let v = v.clamp(ICON_SCALE_MIN, ICON_SCALE_MAX);
    if (v - icon_scale()).abs() < 1e-4 {
        return;
    }
    ICON_SCALE.store(v.to_bits(), Ordering::Relaxed);
    save();
    bump();
}

// ---------- 外观主题 ----------

/// 当前主题（直接转给 `theme::cur()`，设置页下拉用它的下标）
pub fn theme_index() -> usize {
    crate::theme::index()
}

/// 切换主题：改内存索引 + 落盘 + 通知重绘。
///
/// 主题只影响绘制，不需要重建窗口；`version()` 变化后各窗口下一次 timer
/// 轮询就会用新颜色重画一遍。
pub fn set_theme_index(i: usize) {
    if i == crate::theme::index() {
        return;
    }
    crate::theme::set_index(i);
    save();
    bump();
}

/// 取窗口该用哪套 egui 视觉（深色主题用 Dark，浅色主题用 Light）。
/// 用于设置页里的原生控件（滑条、按钮、输入框）跟主题对齐。
pub fn egui_theme() -> eframe::egui::Theme {
    if crate::theme::cur().dark {
        eframe::egui::Theme::Dark
    } else {
        eframe::egui::Theme::Light
    }
}

/// 胶囊锚位/形状
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PillAnchor {
    /// 悬浮在顶部下方一点（圆胶囊）
    Float,
    /// 吸顶刘海：贴屏幕顶，顶部两角方形
    Island,
    /// 吸底：贴屏幕底，底部两角方形
    Bottom,
}

impl PillAnchor {
    fn code(self) -> u32 {
        match self {
            PillAnchor::Float => 1,
            PillAnchor::Island => 2,
            PillAnchor::Bottom => 3,
        }
    }

    fn from_code(c: u32) -> Self {
        match c {
            2 => PillAnchor::Island,
            3 => PillAnchor::Bottom,
            _ => PillAnchor::Float,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "float" | "free" | "悬浮" | "浮动" => Some(PillAnchor::Float),
            "island" | "notch" | "刘海" | "吸顶" => Some(PillAnchor::Island),
            "bottom" | "吸底" | "底部" => Some(PillAnchor::Bottom),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            PillAnchor::Float => "float",
            PillAnchor::Island => "island",
            PillAnchor::Bottom => "bottom",
        }
    }
}

pub fn pill_anchor() -> PillAnchor {
    match PILL_ANCHOR.load(Ordering::Relaxed) {
        UNLOADED => PillAnchor::Float,
        c => PillAnchor::from_code(c),
    }
}

pub fn set_pill_anchor(a: PillAnchor) {
    if a == pill_anchor() {
        return;
    }
    PILL_ANCHOR.store(a.code(), Ordering::Relaxed);
    save();
    bump();
}

// ---------- Dock ----------

/// Dock 靠屏幕哪条边
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DockAnchor {
    Bottom,
    Top,
    Left,
    Right,
}

impl DockAnchor {
    fn code(self) -> u32 {
        match self {
            DockAnchor::Bottom => 1,
            DockAnchor::Top => 2,
            DockAnchor::Left => 3,
            DockAnchor::Right => 4,
        }
    }

    fn from_code(c: u32) -> Self {
        match c {
            2 => DockAnchor::Top,
            3 => DockAnchor::Left,
            4 => DockAnchor::Right,
            _ => DockAnchor::Bottom,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "bottom" | "b" | "下" | "底部" => Some(DockAnchor::Bottom),
            "top" | "t" | "上" | "顶部" => Some(DockAnchor::Top),
            "left" | "l" | "左" | "左侧" => Some(DockAnchor::Left),
            "right" | "r" | "右" | "右侧" => Some(DockAnchor::Right),
            _ => None,
        }
    }

    pub fn is_vertical(self) -> bool {
        matches!(self, DockAnchor::Left | DockAnchor::Right)
    }

    fn as_str(self) -> &'static str {
        match self {
            DockAnchor::Bottom => "bottom",
            DockAnchor::Top => "top",
            DockAnchor::Left => "left",
            DockAnchor::Right => "right",
        }
    }
}

pub fn dock_anchor() -> DockAnchor {
    match DOCK_ANCHOR.load(Ordering::Relaxed) {
        UNLOADED => DockAnchor::Bottom,
        c => DockAnchor::from_code(c),
    }
}

pub fn set_dock_anchor(a: DockAnchor) {
    if a == dock_anchor() {
        return;
    }
    DOCK_ANCHOR.store(a.code(), Ordering::Relaxed);
    save();
    bump();
}

// ---------- 天气 ----------

/// 天气数据源
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WeatherSource {
    Amap,
    OpenMeteo,
}

impl WeatherSource {
    fn code(self) -> u32 {
        match self {
            WeatherSource::Amap => 1,
            WeatherSource::OpenMeteo => 2,
        }
    }

    fn from_code(c: u32) -> Self {
        match c {
            2 => WeatherSource::OpenMeteo,
            _ => WeatherSource::Amap,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().replace(['-', '_'], "").as_str() {
            "amap" | "高德" | "gaode" => Some(WeatherSource::Amap),
            "openmeteo" | "meteo" | "om" => Some(WeatherSource::OpenMeteo),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            WeatherSource::Amap => "amap",
            WeatherSource::OpenMeteo => "open_meteo",
        }
    }
}

pub fn weather_source() -> WeatherSource {
    match WEATHER_SOURCE.load(Ordering::Relaxed) {
        UNLOADED => WeatherSource::Amap,
        c => WeatherSource::from_code(c),
    }
}

pub fn set_weather_source(s: WeatherSource) {
    if s == weather_source() {
        return;
    }
    WEATHER_SOURCE.store(s.code(), Ordering::Relaxed);
    save();
    bump();
}

/// 高德 key（空 = 用编译期默认）
pub fn amap_key() -> String {
    let v = AMAP_KEY.lock().map(|s| s.clone()).unwrap_or_default();
    if v.is_empty() {
        cfg::AMAP_KEY.to_string()
    } else {
        v
    }
}

pub fn set_amap_key(k: &str) {
    let k = k.trim().to_string();
    if k == AMAP_KEY.lock().map(|s| s.clone()).unwrap_or_default() {
        return;
    }
    if let Ok(mut v) = AMAP_KEY.lock() {
        *v = k;
    }
    save();
    bump();
}

// ---------- 邮箱账号 ----------

/// (host, port, user, pass)。设置里没写的字段回落 `data/mail.txt` 的值
/// （mail.rs 首次运行会把编译期默认写进去），所以这里只返回"用户在设置页改过的"。
pub fn mail_overrides() -> (Option<String>, Option<u16>, Option<String>, Option<String>) {
    let host = IMAP_HOST.lock().ok().and_then(|s| {
        if s.trim().is_empty() { None } else { Some(s.trim().to_string()) }
    });
    let port = match IMAP_PORT.load(Ordering::Relaxed) {
        UNLOADED => None,
        c if (1..=65535).contains(&c) => Some(c as u16),
        _ => None,
    };
    let user = IMAP_USER.lock().ok().and_then(|s| {
        if s.trim().is_empty() { None } else { Some(s.trim().to_string()) }
    });
    let pass = IMAP_PASS.lock().ok().and_then(|s| {
        if s.is_empty() { None } else { Some(s.clone()) }
    });
    (host, port, user, pass)
}

pub fn set_mail_account(host: &str, port: u16, user: &str, pass: &str) {
    let cur = mail_overrides();
    let changed = (
        Some(host.trim().to_string()).filter(|s| !s.is_empty()),
        Some(port).filter(|p| (1..=65535).contains(p)),
        Some(user.trim().to_string()).filter(|s| !s.is_empty()),
        Some(pass.to_string()).filter(|s| !s.is_empty()),
    ) != cur;
    if !changed {
        return;
    }
    if let Ok(mut v) = IMAP_HOST.lock() {
        *v = host.trim().to_string();
    }
    IMAP_PORT.store(port.clamp(1, 65535) as u32, Ordering::Relaxed);
    if let Ok(mut v) = IMAP_USER.lock() {
        *v = user.trim().to_string();
    }
    if let Ok(mut v) = IMAP_PASS.lock() {
        *v = pass.to_string();
    }
    save();
    bump();
}

// ---------- 时钟 ----------

pub fn clock_format() -> String {
    CLOCK_FORMAT.lock().map(|s| s.clone()).unwrap_or_default()
}

pub fn set_clock_format(s: &str) {
    let s = s.trim().to_string();
    if s == clock_format() {
        return;
    }
    if let Ok(mut v) = CLOCK_FORMAT.lock() {
        *v = s;
    }
    save();
    bump();
}

/// 音乐条贴在胶囊的哪一侧
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LyricPos {
    Left,
    Right,
    Bottom,
}

impl LyricPos {
    fn code(self) -> u32 {
        match self {
            LyricPos::Left => 1,
            LyricPos::Right => 2,
            LyricPos::Bottom => 3,
        }
    }

    fn from_code(c: u32) -> Self {
        match c {
            1 => LyricPos::Left,
            3 => LyricPos::Bottom,
            _ => LyricPos::Right,
        }
    }

    /// 接受英文键与中文写法，写错了就忽略（保留原值）
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "left" | "l" | "左" | "左侧" => Some(LyricPos::Left),
            "right" | "r" | "右" | "右侧" => Some(LyricPos::Right),
            "bottom" | "b" | "下" | "下方" => Some(LyricPos::Bottom),
            _ => None,
        }
    }
}

fn path() -> PathBuf {
    store::path("settings.txt")
}

/// 启动时调用：文件不存在就写一份默认值，老文件补齐新增键，然后载入
pub fn init() {
    let fresh = ensure_file();
    if !fresh {
        append_missing();
    }
    load();
    MTIME.store(mtime(), Ordering::Relaxed);
}

/// 天气面板字号倍率（乘在 config::WEATHER_* 基准字号上）
pub fn weather_text_scale() -> f32 {
    match SCALE.load(Ordering::Relaxed) {
        UNLOADED => cfg::WEATHER_TEXT_SCALE,
        bits => f32::from_bits(bits),
    }
}

/// 音乐条位置（默认胶囊右侧）
pub fn lyric_position() -> LyricPos {
    match LYRIC_POS.load(Ordering::Relaxed) {
        UNLOADED => LyricPos::Right,
        c => LyricPos::from_code(c),
    }
}

/// 歌词字号（逻辑单位）
pub fn lyric_size() -> f32 {
    match LYRIC_SIZE.load(Ordering::Relaxed) {
        UNLOADED => cfg::LYRIC_FONT_SIZE,
        bits => f32::from_bits(bits),
    }
}

/// Dock 上是否显示托盘图标
pub fn show_tray() -> bool {
    match SHOW_TRAY.load(Ordering::Relaxed) {
        UNLOADED => cfg::SHOW_TRAY,
        c => c == 2,
    }
}

/// Dock 右端是否显示"返回桌面"细长条
pub fn show_desktop_btn() -> bool {
    match SHOW_DESKTOP.load(Ordering::Relaxed) {
        UNLOADED => cfg::SHOW_DESKTOP_BTN,
        c => c == 2,
    }
}

/// 微信托盘图标开始闪烁（新消息）时是否播放内置提示音（不改微信，Orbit 侧伴随提示）
pub fn wechat_sound() -> bool {
    match WECHAT_SOUND.load(Ordering::Relaxed) {
        UNLOADED => true,
        c => c == 2,
    }
}

// ---------- 单个托盘图标的显隐 ----------

/// 被单独隐藏的托盘图标键（`tray::TrayItem::key`）
pub fn hidden_tray() -> Vec<String> {
    HIDE_TRAY.lock().map(|v| v.clone()).unwrap_or_default()
}

/// 这个托盘图标是否被用户单独隐藏了
pub fn tray_hidden(key: &str) -> bool {
    HIDE_TRAY.lock().map(|v| v.iter().any(|k| k == key)).unwrap_or(false)
}

/// 单独隐藏 / 恢复一个托盘图标（key 由 `tray::TrayItem::key` 生成）
pub fn set_tray_hidden(key: &str, hidden: bool) {
    let mut changed = false;
    if let Ok(mut v) = HIDE_TRAY.lock() {
        match (hidden, v.iter().position(|k| k == key)) {
            (true, None) => {
                v.push(key.to_string());
                changed = true;
            }
            (false, Some(i)) => {
                v.remove(i);
                changed = true;
            }
            _ => {}
        }
    }
    if changed {
        save();
        bump();
    }
}

/// 恢复全部被隐藏的托盘图标
pub fn clear_hidden_tray() {
    let changed = HIDE_TRAY.lock().map(|mut v| {
        let had = !v.is_empty();
        v.clear();
        had
    });
    if changed.unwrap_or(false) {
        save();
        bump();
    }
}

/// 设置版本号：任何一项变化都 +1。各窗口记下自己见过的版本，
/// 对不上就重绘——比"谁先看到 mtime 变化"更可靠（设置页面改值不经过 mtime 轮询）。
pub fn version() -> u64 {
    VERSION.load(Ordering::Relaxed)
}

fn bump() {
    VERSION.fetch_add(1, Ordering::Relaxed);
}

// ---------- 写：设置页面用 ----------

pub fn set_weather_text_scale(v: f32) {
    let v = v.clamp(SCALE_MIN, SCALE_MAX);
    if (v - weather_text_scale()).abs() < 1e-4 {
        return;
    }
    SCALE.store(v.to_bits(), Ordering::Relaxed);
    save();
    bump();
}

pub fn set_lyric_size(v: f32) {
    let v = v.clamp(LYRIC_MIN, LYRIC_MAX);
    if (v - lyric_size()).abs() < 1e-4 {
        return;
    }
    LYRIC_SIZE.store(v.to_bits(), Ordering::Relaxed);
    save();
    bump();
}

pub fn set_lyric_position(p: LyricPos) {
    if p == lyric_position() {
        return;
    }
    LYRIC_POS.store(p.code(), Ordering::Relaxed);
    save();
    bump();
}

pub fn set_show_desktop_btn(on: bool) {
    if on == show_desktop_btn() {
        return;
    }
    SHOW_DESKTOP.store(bool_code(on), Ordering::Relaxed);
    save();
    bump();
}

/// 开关微信提示音
pub fn set_wechat_sound(on: bool) {
    if on == wechat_sound() {
        return;
    }
    WECHAT_SOUND.store(bool_code(on), Ordering::Relaxed);
    save();
    bump();
}

/// 按当前值更新设置文件：**只改我们管的键的值**，注释、空行、用户自己加的
/// 其它键都原样保留（手改过的文件不会被洗掉）；缺的键补在末尾。
/// 写完把 mtime 同步掉，免得下一轮轮询又当成"外部改动"重载一次。
fn save() {
    let order = module_order().join(",");
    let fmt = clock_format();
    let key = AMAP_KEY.lock().map(|s| s.clone()).unwrap_or_default();
    let (ih, ip, iu, iw) = (
        IMAP_HOST.lock().map(|s| s.clone()).unwrap_or_default(),
        IMAP_PORT.load(Ordering::Relaxed),
        IMAP_USER.lock().map(|s| s.clone()).unwrap_or_default(),
        IMAP_PASS.lock().map(|s| s.clone()).unwrap_or_default(),
    );
    let mode = |name: &str| module_mode(name).as_str().to_string();
    let vals: Vec<(&str, String)> = vec![
        ("clock_format", fmt),
        ("module_order", order),
        ("weather_mode", mode("weather")),
        ("mail_mode", mode("mail")),
        ("clipboard_mode", mode("clipboard")),
        ("pill_alpha", trim_num(pill_alpha())),
        ("pill_scale", trim_num(pill_scale())),
        ("icon_scale", trim_num(icon_scale())),
        ("theme", crate::theme::cur().key.to_string()),
        ("pill_anchor", pill_anchor().as_str().to_string()),
        ("dock_anchor", dock_anchor().as_str().to_string()),
        ("weather_source", weather_source().as_str().to_string()),
        ("amap_key", key),
        ("imap_host", ih),
        ("imap_port", if ip == UNLOADED { String::new() } else { ip.to_string() }),
        ("imap_user", iu),
        ("imap_pass", iw),
        ("weather_text_scale", trim_num(weather_text_scale())),
        ("lyric_position", lyric_pos_str(lyric_position()).to_string()),
        ("lyric_size", trim_num(lyric_size())),
        ("show_tray", on_off(show_tray()).to_string()),
        ("show_desktop_btn", on_off(show_desktop_btn()).to_string()),
        ("wechat_sound", on_off(wechat_sound()).to_string()),
        ("hide_tray", hidden_tray().join(";")),
    ];
    let raw = std::fs::read_to_string(path()).unwrap_or_default();
    let mut out = String::with_capacity(raw.len() + 128);
    let mut seen: Vec<&str> = Vec::with_capacity(vals.len());
    for line in raw.lines() {
        let t = line.trim();
        let key = if t.is_empty() || t.starts_with('#') {
            None
        } else {
            t.split_once('=').map(|(k, _)| k.trim())
        };
        match key.and_then(|k| vals.iter().find(|(vk, _)| *vk == k)) {
            Some((k, v)) => {
                out.push_str(&format!("{k}={v}\n"));
                seen.push(k);
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    for (k, v) in &vals {
        if !seen.contains(k) {
            out.push_str(&format!("{k}={v}\n"));
        }
    }
    // 先写临时文件再 rename：别的定时器随时可能来读，绝不能让它读到
    // 被截断 / 写了一半的文件（那样会把默认值加载回内存，和文件打架）
    let tmp = path().with_extension("txt.new");
    if std::fs::write(&tmp, out).is_err() {
        return;
    }
    SAVING.store(1, Ordering::SeqCst);
    let _ = std::fs::rename(&tmp, path());
    MTIME.store(mtime(), Ordering::SeqCst);
    SAVING.store(0, Ordering::SeqCst);
}

fn lyric_pos_str(p: LyricPos) -> &'static str {
    match p {
        LyricPos::Left => "left",
        LyricPos::Right => "right",
        LyricPos::Bottom => "bottom",
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "1" } else { "0" }
}

fn bool_code(b: bool) -> u32 {
    if b { 2 } else { 1 }
}

/// 去掉多余的小数 0（1.25 不要写成 1.250000）
fn trim_num(v: f32) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() { "0".to_string() } else { s.to_string() }
}

/// 文件被改过就重新载入；返回 true 表示值确实变了（需要重绘）
pub fn reload_if_changed() -> bool {
    let m = mtime();
    if m == MTIME.load(Ordering::Relaxed) {
        return false;
    }
    // 设置页面正在写盘（临时文件已就位、rename 还没完）：这一轮别读，
    // 顺手把 mtime 记下来，下一轮再看
    if SAVING.load(Ordering::SeqCst) != 0 {
        MTIME.store(m, Ordering::Relaxed);
        return false;
    }
    let before = snapshot();
    load();
    MTIME.store(m, Ordering::Relaxed);
    let changed = before != snapshot();
    if changed {
        bump();
    }
    changed
}

/// 所有可热更新设置的当前值（用来判断"真的变了"）
#[derive(PartialEq)]
struct Snap {
    clock_format: String,
    module_order: Vec<String>,
    weather_mode: ModuleMode,
    mail_mode: ModuleMode,
    clipboard_mode: ModuleMode,
    pill_alpha: f32,
    pill_scale: f32,
    icon_scale: f32,
    theme: usize,
    pill_anchor: PillAnchor,
    dock_anchor: DockAnchor,
    weather_source: WeatherSource,
    amap_key: String,
    imap: (Option<String>, Option<u16>, Option<String>, Option<String>),
    scale: f32,
    lyric_pos: LyricPos,
    lyric_size: f32,
    show_tray: bool,
    show_desktop: bool,
    hidden_tray: Vec<String>,
}

fn snapshot() -> Snap {
    Snap {
        clock_format: clock_format(),
        module_order: module_order(),
        weather_mode: module_mode("weather"),
        mail_mode: module_mode("mail"),
        clipboard_mode: module_mode("clipboard"),
        pill_alpha: pill_alpha(),
        pill_scale: pill_scale(),
        icon_scale: icon_scale(),
        theme: crate::theme::index(),
        pill_anchor: pill_anchor(),
        dock_anchor: dock_anchor(),
        weather_source: weather_source(),
        amap_key: AMAP_KEY.lock().map(|s| s.clone()).unwrap_or_default(),
        imap: mail_overrides(),
        scale: weather_text_scale(),
        lyric_pos: lyric_position(),
        lyric_size: lyric_size(),
        show_tray: show_tray(),
        show_desktop: show_desktop_btn(),
        hidden_tray: hidden_tray(),
    }
}

// ---------- 老格式迁移 ----------

/// 老版 TimeStyle → 格式串片段
fn migrate_time_style(s: &str) -> &'static str {
    match s.trim().to_lowercase().as_str() {
        "24h_s" | "hms" => "%H:%M:%S",
        "12h" => "%p %I:%M",
        "12h_s" => "%p %I:%M:%S",
        "zh" => "%H时%M分",
        _ => "%H:%M",
    }
}

/// 老版 DateStyle → 格式串片段
fn migrate_date_style(s: &str) -> &'static str {
    match s.trim().to_lowercase().as_str() {
        "ymd" | "yyyy_mm_dd" => "%Y-%m-%d",
        "md_wd_dash" | "md_dash" => "%m-%d %a",
        "wd" => "%a",
        "ymd_cn" => "%Y年%m月%d日",
        "md_cn" => "%m月%d日",
        "wd_full" => "%A",
        "ymd_slash" => "%Y/%m/%d",
        _ => "%m月%d日 %a",
    }
}

/// 老版本（三套风格之前）的 6 个主题 key → 新三套风格里最接近的变体。
///
/// 旧 key 已经不在 `theme::ALL` 里，反查会失败；不映射的话用户升级后主题会
/// 悄悄跳回默认，这里给它一个"就近"的落点，至少深浅色不反。
fn legacy_theme_key(old: &str) -> Option<&'static str> {
    match old.trim().to_lowercase().as_str() {
        "glass" | "liquid" => Some("glass2_dark"),
        "mica" => Some("glass2_light"),
        "paper" => Some("calm_light"),
        "neon" => Some("m3_dark"),
        "terminal" => Some("calm_dark"),
        _ => None,
    }
}

/// 老版三件套 → 一条格式串（clock_order/time_format/date_format → clock_format）
fn migrate_clock(order: &str, time: &str, date: &str) -> String {
    let t = migrate_time_style(time);
    let d = migrate_date_style(date);
    match order.trim().to_lowercase().replace(['-', ' '], "_").as_str() {
        "date_time" | "dt" | "日期时间" => format!("{d} {t}"),
        "time" | "t" | "仅时间" => t.to_string(),
        "date" | "d" | "仅日期" => d.to_string(),
        _ => format!("{t} {d}"),
    }
}

/// 老版位置（pill/leftN/rightN）→ (模式, 排序权重)：左为负、右为正
fn migrate_place(s: &str) -> Option<(ModuleMode, i32)> {
    let raw = s.trim().to_lowercase().replace(['-', '_', ' '], "");
    if raw.is_empty() {
        return None;
    }
    let (head, slot) = match raw.chars().last() {
        Some(c) if c.is_ascii_digit() => {
            let n = c.to_digit(10).unwrap_or(1) as i32;
            (&raw[..raw.len() - 1], n)
        }
        _ => (raw.as_str(), 1),
    };
    let mode = match head {
        "pill" | "capsule" | "in" | "胶囊" | "内" => Some((ModuleMode::Pill, 0)),
        "left" | "l" | "左" | "左侧" => Some((ModuleMode::BarLeft, -slot)),
        "right" | "r" | "右" | "右侧" => Some((ModuleMode::BarRight, slot)),
        _ => None,
    }?;
    Some(mode)
}

// ---------- 文件 ----------

const BODY: &str = "\
# Orbit 设置：一行一个 key=value，保存后立即生效
# 顶部胶囊时钟：一条 strftime 风格的格式串（占位符见 docs/ARCHITECTURE.md）：
#   %Y 年 %m 月 %d 日 %H 时(24) %I 时(12) %M 分 %S 秒 %p 上午/下午 %a 周五 %A 星期五 %%
# 例：%Y-%m-%d %H:%M:%S / %H:%M %m月%d日 %a（默认）
clock_format=%H:%M %m月%d日 %a
# 顶部胶囊模块排列（拖动排序写回；时间日期固定在最前，不参与排序）
module_order=weather,mail,clipboard
# 模块摆哪：pill（留在胶囊里）/ left（胶囊左侧一条）/ right（右侧一条）/ off（隐藏）
weather_mode=left
mail_mode=right
clipboard_mode=right
# 胶囊透明度（0.2 ~ 1.0，含分离出去的模块条）
pill_alpha=1.0
# 胶囊整体大小 / Dock 图标大小（1.0 为基准，0.8 ~ 1.35；设置页里有滑条和「重置」）
pill_scale=1
icon_scale=1
# 外观主题（每套都有深色 + 浅色）：calm_light/calm_dark（平静极简）/
# glass2_light/glass2_dark（玻璃拟态 2.0）/ m3_light/m3_dark（Material 3）
theme=calm_light
# 胶囊位置形状：float（悬浮顶部，默认）/ island（吸顶刘海）/ bottom（吸底）
pill_anchor=float
# Dock 靠屏幕哪条边：bottom（默认）/ top / left / right
dock_anchor=bottom
# 天气数据源：amap（高德，默认）/ open_meteo（免 key）
weather_source=amap
# 高德 Web 服务 key（留空用内置默认）
amap_key=
# 邮箱 IMAP 账号（留空的字段回落 data/mail.txt；QQ 邮箱用授权码）
imap_host=
imap_port=
imap_user=
imap_pass=
# 天气面板字号倍率：1.0 为基准
weather_text_scale=1.25
# 音乐条位置：right（胶囊右侧，默认）/ left（左侧）/ bottom（下方）
lyric_position=right
# 歌词字号
lyric_size=14
# 底部 Dock：show_tray / show_desktop_btn 仍可手改（1 = 显示，0 = 隐藏）；
# 设置页里「返回桌面」在托盘图标列表的第一格开关，show_tray 不再提供开关（逐个隐藏用 hide_tray）
show_tray=1
show_desktop_btn=1
# 微信托盘图标开始闪烁（新消息）时播放内置提示音（1 = 开，默认；0 = 关）。
# 想换音效：直接用同名文件覆盖 data/wechat_notify.mp3 即可
wechat_sound=1
# Dock 上要单独隐藏的托盘图标（分号分隔，键由设置页生成；清空 = 全部显示）
hide_tray=
";

/// 文件不存在就写一份默认值；返回 true 表示是新建的
fn ensure_file() -> bool {
    let p = path();
    if p.exists() {
        return false;
    }
    let _ = std::fs::write(p, BODY);
    true
}

/// 老版本的文件缺新键就补到末尾，方便直接编辑
fn append_missing() {
    let p = path();
    let Ok(raw) = std::fs::read_to_string(&p) else { return };
    let mut add = String::new();
    for (key, line) in [
        ("clock_format", "# 顶部胶囊时钟格式串（strftime 风格，占位符见 docs）\nclock_format=%H:%M %m月%d日 %a\n"),
        ("module_order", "# 顶部胶囊模块排列（拖动排序写回）\nmodule_order=weather,mail,clipboard\n"),
        ("weather_mode", "# 模块摆哪：pill / left / right / off\nweather_mode=left\nmail_mode=right\nclipboard_mode=right\n"),
        ("pill_alpha", "# 胶囊透明度（0.2 ~ 1.0）\npill_alpha=1.0\n"),
        ("pill_scale", "# 胶囊 / Dock 图标大小（1.0 为基准，0.8 ~ 1.35）\npill_scale=1\nicon_scale=1\n"),
        (
            "theme",
            "# 外观主题：calm_light/calm_dark（平静极简）/ glass2_light/glass2_dark（玻璃 2.0）\n\
             # m3_light/m3_dark（Material 3）；设置页里点色块切换\ntheme=calm_light\n",
        ),
        ("pill_anchor", "# 胶囊位置形状：float / island（吸顶刘海）/ bottom（吸底）\npill_anchor=float\n"),
        ("dock_anchor", "# Dock 靠屏幕哪条边：bottom / top / left / right\ndock_anchor=bottom\n"),
        ("weather_source", "# 天气数据源：amap / open_meteo\nweather_source=amap\namap_key=\n"),
        ("imap_host", "# 邮箱 IMAP 账号（留空回落 data/mail.txt）\nimap_host=\nimap_port=\nimap_user=\nimap_pass=\n"),
        ("lyric_position", "# 音乐条位置：right（胶囊右侧，默认）/ left（左侧）/ bottom（下方）\nlyric_position=right\n"),
        ("lyric_size", "# 歌词字号\nlyric_size=14\n"),
        ("weather_text_scale", "# 天气面板字号倍率：1.0 为基准\nweather_text_scale=1.25\n"),
        (
            "show_tray",
            "# 底部 Dock：显示托盘图标 / 右端“返回桌面”细长条（1 = 显示，0 = 隐藏）\nshow_tray=1\nshow_desktop_btn=1\n",
        ),
        ("hide_tray", "# Dock 上要单独隐藏的托盘图标（分号分隔，键由设置页生成；清空 = 全部显示）\nhide_tray=\n"),
        ("wechat_sound", "# 微信托盘图标开始闪烁（新消息）时播放内置提示音（1 = 开，默认；0 = 关）\nwechat_sound=1\n"),
    ] {
        if !raw.contains(key) {
            add.push_str(line);
        }
    }
    if !add.is_empty() {
        let _ = std::fs::write(&p, format!("{raw}{add}"));
    }
}

fn load() {
    let raw = std::fs::read_to_string(path()).unwrap_or_default();
    // 读到空文件（极端情况：外部工具正在重写）就当没读到，保留内存里的值，
    // 否则会把默认值灌回内存，和文件内容打架
    if raw.trim().is_empty() {
        return;
    }
    let mut scale = cfg::WEATHER_TEXT_SCALE;
    let mut size = cfg::LYRIC_FONT_SIZE;
    let mut pos = LyricPos::Right;
    let mut fmt: Option<String> = None;
    // 老键迁移用
    let mut old_order: Option<String> = None;
    let mut old_time: Option<String> = None;
    let mut old_date: Option<String> = None;
    let mut modes: Vec<(String, ModuleMode, i32)> = Vec::new();
    let mut tray = cfg::SHOW_TRAY;
    let mut desktop = cfg::SHOW_DESKTOP_BTN;
    let mut wsound = true;
    let mut hide_tray: Vec<String> = Vec::new();
    let mut alpha = 1.0f32;
    let mut pscale = 1.0f32;
    let mut iscale = 1.0f32;
    let mut tkey: String = crate::theme::cur().key.to_string();
    let mut panchor = PillAnchor::Float;
    let mut danchor = DockAnchor::Bottom;
    let mut wsource = WeatherSource::Amap;
    let mut akey: Option<String> = None;
    let mut ihost: Option<String> = None;
    let mut iport: Option<u16> = None;
    let mut iuser: Option<String> = None;
    let mut ipass: Option<String> = None;
    let mut order: Option<Vec<String>> = None;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "clock_format" => fmt = Some(v.to_string()),
            "clock_order" => old_order = Some(v.to_string()),
            "time_format" => old_time = Some(v.to_string()),
            "date_format" => old_date = Some(v.to_string()),
            "module_order" => {
                let list: Vec<String> = v
                    .split(',')
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| MODULES.contains(&s.as_str()))
                    .collect();
                if !list.is_empty() {
                    order = Some(list);
                }
            }
            "weather_mode" | "mail_mode" | "clipboard_mode" => {
                if let Some(m) = ModuleMode::parse(v) {
                    modes.push((k.trim_end_matches("_mode").to_string(), m, 0));
                }
            }
            // 老位置键：迁移成 mode + 排序权重
            "weather_pos" | "mail_pos" | "clipboard_pos" => {
                if let Some((m, w)) = migrate_place(v) {
                    modes.push((k.trim_end_matches("_pos").to_string(), m, w));
                }
            }
            "pill_alpha" => {
                if let Some(n) = parse_num(v) {
                    alpha = n.clamp(ALPHA_MIN, ALPHA_MAX);
                }
            }
            "pill_scale" => {
                if let Some(n) = parse_num(v) {
                    pscale = n.clamp(PILL_SCALE_MIN, PILL_SCALE_MAX);
                }
            }
            "icon_scale" => {
                if let Some(n) = parse_num(v) {
                    iscale = n.clamp(ICON_SCALE_MIN, ICON_SCALE_MAX);
                }
            }
            "theme" => {
                if crate::theme::index_of(v).is_some() {
                    tkey = v.to_string();
                } else if let Some(new) = legacy_theme_key(v) {
                    // 老版本的 6 套风格已删除：就近映射到新三套里最像的一个变体
                    tkey = new.to_string();
                }
            }
            "pill_anchor" => {
                if let Some(a) = PillAnchor::parse(v) {
                    panchor = a;
                }
            }
            "dock_anchor" => {
                if let Some(a) = DockAnchor::parse(v) {
                    danchor = a;
                }
            }
            "weather_source" => {
                if let Some(s) = WeatherSource::parse(v) {
                    wsource = s;
                }
            }
            "amap_key" => akey = Some(v.to_string()),
            "imap_host" => ihost = Some(v.to_string()),
            "imap_port" => {
                if let Some(n) = parse_num(v) {
                    iport = Some(n as u16);
                }
            }
            "imap_user" => iuser = Some(v.to_string()),
            "imap_pass" => ipass = Some(v.to_string()),
            "hide_tray" => hide_tray = parse_hide(v),
            "weather_text_scale" => {
                if let Some(n) = parse_num(v) {
                    scale = n.clamp(SCALE_MIN, SCALE_MAX);
                }
            }
            "lyric_size" => {
                if let Some(n) = parse_num(v) {
                    size = n.clamp(LYRIC_MIN, LYRIC_MAX);
                }
            }
            "lyric_position" => {
                if let Some(p) = LyricPos::parse(v) {
                    pos = p;
                }
            }
            "show_tray" => {
                if let Some(b) = parse_bool(v) {
                    tray = b;
                }
            }
            "show_desktop_btn" => {
                if let Some(b) = parse_bool(v) {
                    desktop = b;
                }
            }
            "wechat_sound" => {
                if let Some(b) = parse_bool(v) {
                    wsound = b;
                }
            }
            _ => {}
        }
    }

    // 时钟格式：新键优先；没有就从老三件套迁移（都不在就用默认）
    let fmt = match fmt {
        Some(f) if !f.is_empty() => f,
        _ => match (old_order.as_deref(), old_time.as_deref(), old_date.as_deref()) {
            (Some(o), Some(t), Some(d)) => migrate_clock(o, t, d),
            _ => "%H:%M %m月%d日 %a".to_string(),
        },
    };

    // 模块排列：新键优先；没有就按老位置键的权重排（左负右正，贴近胶囊在前）
    let order = order.unwrap_or_else(|| {
        let mut by_weight: Vec<(i32, usize)> = Vec::new();
        for (i, (_, _, w)) in modes.iter().enumerate() {
            if *w != 0 {
                by_weight.push((*w, i));
            }
        }
        by_weight.sort_by_key(|(w, _)| *w);
        let mut list: Vec<String> = Vec::new();
        // pill 模块放最前（排在时钟后面），再按权重排分离的
        for (name, m, _) in &modes {
            if *m == ModuleMode::Pill && !list.iter().any(|s| s == name) {
                list.push(name.clone());
            }
        }
        for (_, i) in by_weight {
            let (name, _, _) = &modes[i];
            if !list.iter().any(|s| s == name) {
                list.push(name.clone());
            }
        }
        for m in MODULES {
            if !list.iter().any(|s| s == m) {
                list.push(m.to_string());
            }
        }
        list
    });

    // 模式：老位置键与新模式键都写了时，新模式键优先（modes 顺序 = 文件行序）
    for (name, m, w) in &modes {
        // 老键迁移出来的带权重；带权重且已被新键覆盖（新键在同名老键之后出现）就跳过
        let _ = w;
        set_mode_static(name, m.code());
    }

    SCALE.store(scale.to_bits(), Ordering::Relaxed);
    LYRIC_SIZE.store(size.to_bits(), Ordering::Relaxed);
    LYRIC_POS.store(pos.code(), Ordering::Relaxed);
    if let Ok(mut v) = CLOCK_FORMAT.lock() {
        *v = fmt;
    }
    if let Ok(mut v) = MODULE_ORDER.lock() {
        *v = order;
    }
    PILL_ALPHA.store(alpha.to_bits(), Ordering::Relaxed);
    PILL_SCALE.store(pscale.to_bits(), Ordering::Relaxed);
    ICON_SCALE.store(iscale.to_bits(), Ordering::Relaxed);
    // 主题：所有绘制都走 theme::cur()，存一次索引即可，不必逐个窗口通知
    if let Some(i) = crate::theme::index_of(&tkey) {
        crate::theme::set_index(i);
    }
    PILL_ANCHOR.store(panchor.code(), Ordering::Relaxed);
    DOCK_ANCHOR.store(danchor.code(), Ordering::Relaxed);
    WEATHER_SOURCE.store(wsource.code(), Ordering::Relaxed);
    if let Some(k) = akey {
        if let Ok(mut v) = AMAP_KEY.lock() {
            *v = k;
        }
    }
    if let Some(h) = ihost {
        if let Ok(mut v) = IMAP_HOST.lock() {
            *v = h;
        }
    }
    if let Some(p) = iport {
        IMAP_PORT.store(p.clamp(1, 65535) as u32, Ordering::Relaxed);
    }
    if let Some(u) = iuser {
        if let Ok(mut v) = IMAP_USER.lock() {
            *v = u;
        }
    }
    if let Some(p) = ipass {
        if let Ok(mut v) = IMAP_PASS.lock() {
            *v = p;
        }
    }
    SHOW_TRAY.store(bool_code(tray), Ordering::Relaxed);
    SHOW_DESKTOP.store(bool_code(desktop), Ordering::Relaxed);
    WECHAT_SOUND.store(bool_code(wsound), Ordering::Relaxed);
    if let Ok(mut v) = HIDE_TRAY.lock() {
        *v = hide_tray;
    }
}

/// 布尔：1/0、true/false、on/off、开/关
fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_lowercase().as_str() {
        "1" | "true" | "on" | "yes" | "开" => Some(true),
        "0" | "false" | "off" | "no" | "关" => Some(false),
        _ => None,
    }
}

fn parse_num(v: &str) -> Option<f32> {
    v.trim().parse::<f32>().ok().filter(|n| n.is_finite())
}

/// 隐藏列表写盘用 `;` 分隔；读到空值 = 全部显示
fn parse_hide(v: &str) -> Vec<String> {
    v.split(';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn mtime() -> u64 {
    std::fs::metadata(path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_both_languages() {
        assert_eq!(LyricPos::parse("LEFT"), Some(LyricPos::Left));
        assert_eq!(LyricPos::parse(" 下方 "), Some(LyricPos::Bottom));
        assert_eq!(LyricPos::parse("middle"), None);
        assert_eq!(PillAnchor::parse("刘海"), Some(PillAnchor::Island));
        assert_eq!(DockAnchor::parse("TOP"), Some(DockAnchor::Top));
        assert_eq!(DockAnchor::parse("右侧"), Some(DockAnchor::Right));
        assert_eq!(ModuleMode::parse("隐藏"), Some(ModuleMode::Off));
        assert_eq!(WeatherSource::parse("open-meteo"), Some(WeatherSource::OpenMeteo));
        assert_eq!(WeatherSource::parse("高德"), Some(WeatherSource::Amap));
    }

    #[test]
    fn unknown_code_falls_back() {
        assert_eq!(LyricPos::from_code(99), LyricPos::Right);
        assert_eq!(DockAnchor::from_code(9), DockAnchor::Bottom);
        assert_eq!(PillAnchor::from_code(9), PillAnchor::Float);
        assert_eq!(ModuleMode::from_code(9), ModuleMode::Pill);
    }

    #[test]
    fn module_mode_roundtrip() {
        for m in [ModuleMode::Pill, ModuleMode::BarLeft, ModuleMode::BarRight, ModuleMode::Off] {
            assert_eq!(ModuleMode::parse(m.as_str()), Some(m));
            assert_eq!(ModuleMode::from_code(m.code()), m);
        }
    }

    #[test]
    fn migration() {
        assert_eq!(migrate_clock("date_time", "24h", "ymd"), "%Y-%m-%d %H:%M");
        assert_eq!(migrate_clock("time_date", "24h_s", "md_wd"), "%H:%M:%S %m月%d日 %a");
        assert_eq!(migrate_clock("date", "12h", "ymd_cn"), "%Y年%m月%d日");
        assert_eq!(
            migrate_place("left2"),
            Some((ModuleMode::BarLeft, -2))
        );
        assert_eq!(migrate_place("pill"), Some((ModuleMode::Pill, 0)));
        assert_eq!(migrate_place("right3"), Some((ModuleMode::BarRight, 3)));
        assert_eq!(migrate_place("左下"), None);
    }

    #[test]
    fn legacy_theme_keys_map_to_new_styles() {
        // 老的 6 套风格已删除：深浅色不能反，且映射结果必须是现在真实存在的 key
        for (old, want_dark) in [
            ("glass", true),
            ("liquid", true),
            ("mica", false),
            ("paper", false),
            ("neon", true),
            ("terminal", true),
        ] {
            let new = legacy_theme_key(old).unwrap_or_else(|| panic!("{old} 没映射"));
            let i = crate::theme::index_of(new).unwrap_or_else(|| panic!("{new} 不存在"));
            assert_eq!(crate::theme::ALL[i].dark, want_dark, "{old} → {new} 深浅反了");
        }
        // 不认识的值不算老 key（交给默认主题）
        assert_eq!(legacy_theme_key("calm_light"), None);
        assert_eq!(legacy_theme_key("随便写的"), None);
    }

    #[test]
    fn bool_parsing_and_formatting() {
        assert_eq!(parse_bool("1"), Some(true));
        assert_eq!(parse_bool(" OFF "), Some(false));
        assert_eq!(parse_bool("开"), Some(true));
        assert_eq!(parse_bool("maybe"), None);
        // 隐藏列表：分号分隔、空值 = 全部显示
        assert!(parse_hide("").is_empty());
        assert_eq!(parse_hide(" g:1 ; e:c:\\a.exe#0 "), vec!["g:1".to_string(), "e:c:\\a.exe#0".to_string()]);
        // 写盘时不要 1.250000 这种尾巴
        assert_eq!(trim_num(1.25), "1.25");
        assert_eq!(trim_num(14.0), "14");
        assert_eq!(trim_num(0.0), "0");
    }
}
