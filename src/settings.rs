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
//! - `alpha`              **界面透明度**（统一一个值，0.2 ~ 1.0，默认 1.0）：胶囊（含分离
//!                         模块条 / 音乐条）/ Dock / 展开面板 / 开始菜单 / 横幅一起调，
//!                         天气卡片不受影响。**只调板面底色的 alpha，文字和图标不参与**：
//!                         拉到 100% 底色完全不透明，拉低只让背景透出桌面，
//!                         见 `config::bg_alpha` / `config::ui_bg_effective`。
//!                         老版本的 `pill_alpha` / `dock_alpha` 仍会被读取并合并成这一条。
//! - `pill_scale`         胶囊（含模块条 / 音乐条）整体大小 0.8 ~ 1.35，默认 1.0。
//!                         1.0 = 滑条 100%，实际倍率是"老的基准 × 0.85"
//! - `icon_scale`         Dock 图标（连同 Dock 高度与托盘图标）大小 0.8 ~ 1.35，默认 1.0。
//!                         1.0 = 滑条 100%，实际倍率是"老的基准 × 0.95"
//! - `corner_radius`      统一圆角半径（逻辑点，0 ~ 22，默认 14）：
//!                         顶部胶囊 / 音乐条 / 模块条 / Dock 共用；默认是圆角矩形，
//!                         调到条高一半即完全胶囊形
//! - `theme`              外观主题 key（`calm_light` / `calm_dark` / `glass2_light` /
//!                         `glass2_dark` / `m3_light` / `m3_dark`）；三套风格各带深浅色。
//!                         老版本的 `glass`/`liquid`/`mica`/`paper`/`neon`/`terminal`
//!                         会被 `legacy_theme_key` 就近映射到新 key
//! - `pill_anchor`        胶囊位置/形状：`float`（悬浮顶部，默认）/ `island`（吸顶刘海，
//!                          贴屏幕顶、顶边两角张开成"耳"）。老值 `bottom`（吸底）已取消，
//!                          读到就按 `float` 处理
//! - `dock_anchor`        底部 Dock 位置：`bottom`（默认）/ `top` / `left` / `right`
//! - `weather_source`     天气数据源：`amap`（高德，默认）/ `open_meteo`
//! - `amap_key`           高德 Web 服务 key（空 = 回落编译期默认）
//! - `imap_host` / `imap_port` / `imap_user` / `imap_pass`
//!   邮箱 IMAP 账号（设置页可改；空值回落 `data/mail.txt`，再回落编译期默认）
//! - `weather_text_scale` 天气面板字号倍率（1.0 = 基准，0.8 ~ 1.8）
//! - `lyric_position`     音乐条位置：`right`（默认，贴胶囊右侧）/ `left` / `bottom`
//! - `lyric_size`         歌词字号（逻辑单位，10 ~ 26，默认 14）
//! - `lyric_lead`         歌词提前量（-5 ~ 5 秒，默认 1；0 = 不提前，负值 = 延后）
//! - `show_tray`          Dock 上是否显示托盘图标（0 = 隐藏；仅手改文件生效）
//! - `wechat_sound`       微信托盘图标闪烁（新消息）时播放内置提示音（0 = 关，默认开）
//! - `wechat_cards`       微信消息卡片（UIA 读会话名+摘要）：`0` = 关（默认，托盘闪烁
//!                          直接弹通用横幅）；`1` = 开，读取线程随开关启停
//! - `wechat_ignore`      卡片白名单：标题命中就不弹（英文逗号分隔，默认「折叠的聊天」）
//! - `wechat_banner_jump`  点击微信消息横幅是否直达对应会话：`1` = 开；`0` = 关（默认）
//! - `wechat_hover_preview` 微信托盘悬停预览浮窗（自绘，替代微信自己的小窗）：
//!                        `1` = 开；`0` = 关（默认，保留微信原生悬停小窗）
//! - `wechat_hotkey_bind` 微信热键「按账号」绑定：`账号ID=热键`，英文逗号分隔。
//!                        绑到账号跟账号走（换启动顺序不串），设置页账号列表每行直接填
//! - `wechat_hotkeys`     微信实例的「显示↔最小化」热键（英文逗号分隔，按实例创建
//!                          先后对应：第 1 个 = 先开的那个微信）。设置页可直接改。
//!                          空 = 不注册任何热键（默认）。
//!                          例：`F1,F2`、`Ctrl+Alt+1,Ctrl+Alt+2`。**永远只做最小化，
//!                          绝不 hide**——隐藏到托盘后 UIA 树会被微信整棵摘掉，
//!                          正文/未读就再也读不到了。
//! - `wechat_close_min`   点微信右上角「关闭」时**改成最小化**而不是收进托盘
//!                          （0 = 关，默认开）。低级鼠标钩子吞掉那一下点击，
//!                          从源头杜绝「误点关闭 → 隐藏 → 读不到数据」。
//! - `font`               界面字体：`misans`（默认，随包分发 MiSans Semibold）/ `system`（微软雅黑等系统字体）
//! - `show_desktop_btn`   Dock"返回桌面"细长条（0 = 不显示；设置页在「托盘图标」
//!                          列表的"返回桌面"格里开关）
//! - `hide_tray`          Dock 上要**单独隐藏**的托盘图标（分号分隔，键由程序生成）
//! - `icon_over`          Dock 图标指定（分号分隔的 `键|exe全路径`）：键是托盘图标键
//!                         或 `app:<exe小写>`；值是拿图标的 exe / .lnk
//! - `dock_pins`          Dock 固定图标（分号分隔的 exe / .lnk 全路径，按顺序显示）。
//!                          **键不存在**时回落到系统任务栏的钉选；一旦在设置页增删过
//!                          就以这份列表为准（空值 = 一个都不钉）
//!
//! 设置页面（`settingsui.rs`）改值走的也是这里：`set_*` 更新内存 + 重写文件，
//! 并把 `VERSION` 加一；各窗口在自己的定时器里比对版本号决定是否重绘。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
/// 歌词提前量上限（秒，可正可负）
pub const LYRIC_LEAD_MAX: f32 = 5.0;
/// 统一圆角半径上下限（逻辑点）。0 = 直角，到条高一半就成了完全胶囊形
pub const CORNER_MIN: f32 = 0.0;
pub const CORNER_MAX: f32 = 22.0;
/// 圆角半径默认值：圆角矩形（不是胶囊）
const CORNER_DEFAULT: f32 = 14.0;
/// 界面透明度上下限（再低就看不清了）；滑条量程直接用这两个值。
/// 一个值管所有 Orbit 界面（天气卡片除外）：胶囊 / 模块条 / 音乐条 / Dock /
/// 展开面板 / 开始菜单 / 横幅。
pub const ALPHA_MIN: f32 = 0.2;
pub const ALPHA_MAX: f32 = 1.0;
/// 胶囊（含模块条 / 音乐条）整体缩放上下限。1.0 = 基准尺寸。
/// 缩放的是"逻辑点"而不是像素：文字与图标按新尺寸重新排版光栅化，不会发虚。
pub const PILL_SCALE_MIN: f32 = 0.80;
pub const PILL_SCALE_MAX: f32 = 1.35;
/// 滑条"100%"对应的实际倍率：胶囊基准整体收到原来的 85%
/// （用户要求"默认百分百 = 现在 85% 的大小"，滑条数值本身仍从 1.0 开始算）
const PILL_SCALE_BASE: f32 = 0.85;
/// Dock 图标缩放上下限（Dock 高度 / 槽位 / 托盘图标随之等比变化）
pub const ICON_SCALE_MIN: f32 = 0.80;
pub const ICON_SCALE_MAX: f32 = 1.35;
/// 滑条"100%"对应的实际倍率：Dock 整体收到原来的 95%
const ICON_SCALE_BASE: f32 = 0.95;
/// 右下角横幅整体大小上下限：1.0 = 当前默认卡片尺寸（660×138 逻辑点）
pub const BANNER_SCALE_MIN: f32 = 0.80;
pub const BANNER_SCALE_MAX: f32 = 1.50;
/// 滑条"100%"对应的实际倍率：横幅沿用胶囊时代的 85% 基准，100% = 发布版实际大小
const BANNER_SCALE_BASE: f32 = 0.85;
/// AtomicU32 的 0 表示"还没加载"
const UNLOADED: u32 = 0;

static SCALE: AtomicU32 = AtomicU32::new(UNLOADED);
static LYRIC_SIZE: AtomicU32 = AtomicU32::new(UNLOADED);
/// 歌词提前量（秒，f32 bits；UNLOADED 时默认 1 秒）
static LYRIC_LEAD: AtomicU32 = AtomicU32::new(UNLOADED);
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
/// 界面透明度（f32 位型，0.2 ~ 1.0；胶囊 / Dock / 面板 / 开始菜单 / 横幅共用）
static UI_ALPHA: AtomicU32 = AtomicU32::new(UNLOADED);
/// 胶囊缩放 / Dock 图标缩放（f32 位型）
static PILL_SCALE: AtomicU32 = AtomicU32::new(UNLOADED);
static ICON_SCALE: AtomicU32 = AtomicU32::new(UNLOADED);
static BANNER_SCALE: AtomicU32 = AtomicU32::new(UNLOADED);
static PILL_ANCHOR: AtomicU32 = AtomicU32::new(UNLOADED);
static DOCK_ANCHOR: AtomicU32 = AtomicU32::new(UNLOADED);
/// 统一圆角半径（f32 位型）：胶囊 / 音乐条 / 模块条 / Dock 共用
static CORNER: AtomicU32 = AtomicU32::new(UNLOADED);
/// 天气数据源编码 / 高德 key
static WEATHER_SOURCE: AtomicU32 = AtomicU32::new(UNLOADED);
/// 天气卡片配色方案：0 = 自动跟壁纸（默认）/ 1 = 深色背景（浅色字）/ 2 = 浅色背景（深色字）
static WEATHER_SCHEME: AtomicU32 = AtomicU32::new(UNLOADED);
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
/// 微信新消息是否在屏幕右下角弹会话名+摘要小卡片（只读 UIA）
static WECHAT_CARDS: AtomicU32 = AtomicU32::new(UNLOADED);
/// 微信托盘悬停预览浮窗（默认关：默认保留微信自己的悬停小窗）
static WECHAT_HOVER: AtomicU32 = AtomicU32::new(UNLOADED);
/// 点击微信消息横幅是否直达对应会话（默认开）
static WECHAT_JUMP: AtomicU32 = AtomicU32::new(UNLOADED);
/// 右下角横幅每条停留秒数；0 = 不自动关闭（UNLOADED 时默认 6）
static BANNER_SECS: AtomicU32 = AtomicU32::new(UNLOADED);
/// 微信卡片标题白名单：标题命中就不弹（默认含「折叠的聊天」）
static WECHAT_IGNORE: LazyLock<Mutex<Vec<String>>> =
    LazyLock::new(|| Mutex::new(vec!["折叠的聊天".to_string()]));
/// 微信实例的「显示↔最小化」热键（按实例创建先后；空 = 不注册）
static WECHAT_HOTKEYS: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// 微信热键的「按账号」绑定：(账号快照 id, 热键)。跟账号走，换启动顺序不会串；
/// 没有快照的实例仍走 `WECHAT_HOTKEYS` 的启动顺序。
static WECHAT_HOTKEY_BIND: LazyLock<Mutex<Vec<(String, String)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
/// 微信主程序路径（Weixin.exe；空 = 自动探测：运行中进程 → 注册表 → 常见位置）
static WECHAT_PATH: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));
/// 微信数据目录（xwechat_files；空 = 自动探测：安装目录同级 → 注册表）
static WECHAT_DATA_DIR: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));
/// 点微信「关闭」是否改成最小化（UNLOADED / 1 = 关 / 2 = 开，默认开）
static WECHAT_CLOSE_MIN: AtomicU32 = AtomicU32::new(UNLOADED);
/// 界面字体：UNLOADED / 1 = MiSans（随包分发，磁盘加载）/ 2 = 系统字体
static FONT: AtomicU32 = AtomicU32::new(UNLOADED);
/// 被单独隐藏的托盘图标键（`tray::TrayItem::key`）。数量很少，读写都走这把锁
static HIDE_TRAY: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Dock 固定图标（exe / .lnk 全路径，按顺序显示）。
/// `None` = 用户还没在设置页改过 → 由 dock 回落到系统任务栏的钉选
static DOCK_PINS: Mutex<Option<Vec<String>>> = Mutex::new(None);
/// 开始菜单快捷方式（(名称, 程序路径)，按顺序显示）。
/// `None` = 用户还没在设置页改过 → 默认只有「命令提示符 (cmd)」。
/// 设置页的「快速添加 → 任务管理器」（`taskmgr_path`）也是往这里 push 一条。
static START_PINS: Mutex<Option<Vec<(String, String)>>> = Mutex::new(None);
/// 开机启动（0 = 未加载，1 = 关，2 = 开；注册表 HKCU Run 键，见 guard.rs）
static AUTOSTART: AtomicU32 = AtomicU32::new(UNLOADED);
/// 退出守护：orbit 被强杀后由守护子进程重启 explorer 恢复任务栏（默认开）
static GUARDIAN: AtomicU32 = AtomicU32::new(UNLOADED);
/// 伪造托盘点击/悬停（traymsg 回调消息投递）：关闭后完全交给已转发的 Explorer 托盘
static TRAY_FAKE_INPUT: AtomicU32 = AtomicU32::new(UNLOADED);
/// Dock 内建「系统控制」按钮（音量/亮度/WiFi/蓝牙）：默认隐藏，设置页 Dock 区可开
static SYS_PANEL: AtomicU32 = AtomicU32::new(UNLOADED);
/// 图标指定：键（托盘图标键 / `app:<exe>`）→ 取图标的 exe/.lnk 全路径。
/// 有些程序注册的托盘 hIcon 只有 16px，放大到 Dock 尺寸必然糊，用户可以指定一个
/// 图标清晰的 exe 顶替它。
static ICON_OVER: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// 托盘图标自定义排序（`tray::TrayItem::key` 列表，设置页 ▲▼ 排序写回）。
/// 空 = 用户没自定义，托盘按默认规则（首次出现顺序）排；非空时表里的键按
/// 自定义顺序排，没进表的（新图标）按默认规则跟在后面。
static TRAY_ORDER: Mutex<Vec<String>> = Mutex::new(Vec::new());

// ---------- 通知 / 横幅 / 提示音（按来源独立配置）----------
/// 通知通道：0 = 关，1 = Orbit 横幅，2 = Windows 系统通知。见 `NotifyChannel`
static NOTIFY_WECHAT: AtomicU32 = AtomicU32::new(UNLOADED);
static NOTIFY_TRAY: AtomicU32 = AtomicU32::new(UNLOADED);
static NOTIFY_MAIL: AtomicU32 = AtomicU32::new(UNLOADED);
static NOTIFY_CLIPBOARD: AtomicU32 = AtomicU32::new(UNLOADED);
/// 各来源横幅停留秒数（存「秒+1」避开 UNLOADED 哨兵，0 秒 = 常驻）
static BANNER_WECHAT_SECS: AtomicU32 = AtomicU32::new(UNLOADED);
static BANNER_MAIL_SECS: AtomicU32 = AtomicU32::new(UNLOADED);
static BANNER_TRAY_SECS: AtomicU32 = AtomicU32::new(UNLOADED);
static BANNER_CLIPBOARD_SECS: AtomicU32 = AtomicU32::new(UNLOADED);
/// 提示音模式：0 = 关，1 = 单次，2 = 循环 2 次，3 = 循环 3 次，4 = 直到消息消失。见 `SoundMode`
static SOUND_MODE: AtomicU32 = AtomicU32::new(UNLOADED);
/// 接管 Windows 通知中心（0 = 未加载/关，2 = 开；默认关）
static NOTIFY_TAKEOVER: AtomicU32 = AtomicU32::new(UNLOADED);

// ---------- 邮件多账号 ----------
/// 多账号列表：每个账号 `host|port|user|pass`，账号间用 `;` 分隔。
/// 非空时优先于单账号 imap_* 键，mail.rs 会同时监听这里面所有账号。
static MAIL_ACCOUNTS: Mutex<Vec<MailAccount>> = Mutex::new(Vec::new());
/// 已测试连通的账号标识（`user@host`）列表，设置页打勾标记用，避免每次重测
static MAIL_VERIFIED: Mutex<Vec<String>> = Mutex::new(Vec::new());

// ---------- 剪切板 / Dock / 开始菜单 ----------
/// 剪切板模块常驻置顶文字（在面板顶部固定显示一条）
static CLIPBOARD_PINNED_TEXT: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));
/// Win 键接管开始菜单（0 = 未加载，1 = 关，2 = 开，默认开）
static START_HOOK: AtomicU32 = AtomicU32::new(UNLOADED);
/// 开始菜单每行快捷方式数量（1 ~ 8，默认 4）
static START_COLS: AtomicU32 = AtomicU32::new(UNLOADED);
/// 设置版本号：任何值变化（手改文件或设置页面改）都 +1，各窗口据此判断要不要重绘
static VERSION: AtomicU64 = AtomicU64::new(1);
/// 正在写盘：这时读文件可能拿到半成品，轮询先跳过
static SAVING: AtomicU64 = AtomicU64::new(0);
static MTIME: AtomicU64 = AtomicU64::new(0);
/// 编辑会话中：设置页点「应用」前，所有 `set_*` 只改内存、不落盘不 bump，
/// 改完由 `commit_edit()` 一次性 save()+bump()。见下方三个函数。
static EDITING: AtomicBool = AtomicBool::new(false);

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

/// 通知通道：某类消息来了走哪条路。
/// Orbit 横幅 = 自绘分层横幅；Windows = 系统通知中心 Toast；Off = 不弹。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotifyChannel {
    Off,
    Orbit,
    Windows,
}

impl NotifyChannel {
    fn code(self) -> u32 {
        match self {
            NotifyChannel::Off => 1,
            NotifyChannel::Orbit => 2,
            NotifyChannel::Windows => 3,
        }
    }
    fn from_code(c: u32) -> Self {
        match c {
            3 => NotifyChannel::Windows,
            2 => NotifyChannel::Orbit,
            _ => NotifyChannel::Off,
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "off" | "none" | "关" | "不通知" => Some(NotifyChannel::Off),
            "orbit" | "banner" | "横幅" => Some(NotifyChannel::Orbit),
            "windows" | "toast" | "系统" => Some(NotifyChannel::Windows),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            NotifyChannel::Off => "off",
            NotifyChannel::Orbit => "orbit",
            NotifyChannel::Windows => "windows",
        }
    }
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            NotifyChannel::Off => "不通知",
            NotifyChannel::Orbit => "Orbit 横幅",
            NotifyChannel::Windows => "Windows 通知",
        }
    }
    #[allow(dead_code)]
    pub const ALL: [NotifyChannel; 3] = [NotifyChannel::Off, NotifyChannel::Orbit, NotifyChannel::Windows];
}

/// 提示音模式：单次 / 循环 N 次 / 直到消息消失。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SoundMode {
    Off,
    Once,
    Twice,
    Thrice,
    UntilDismiss,
}

impl SoundMode {
    fn code(self) -> u32 {
        match self {
            SoundMode::Off => 1,
            SoundMode::Once => 2,
            SoundMode::Twice => 3,
            SoundMode::Thrice => 4,
            SoundMode::UntilDismiss => 5,
        }
    }
    fn from_code(c: u32) -> Self {
        match c {
            2 => SoundMode::Once,
            3 => SoundMode::Twice,
            4 => SoundMode::Thrice,
            5 => SoundMode::UntilDismiss,
            _ => SoundMode::Off,
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "off" | "none" | "关" => Some(SoundMode::Off),
            "once" | "single" | "单次" | "一声" => Some(SoundMode::Once),
            "twice" | "2" | "两次" | "两声" => Some(SoundMode::Twice),
            "thrice" | "3" | "三次" | "三声" => Some(SoundMode::Thrice),
            "until" | "loop" | "直到消失" | "循环" => Some(SoundMode::UntilDismiss),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            SoundMode::Off => "off",
            SoundMode::Once => "once",
            SoundMode::Twice => "twice",
            SoundMode::Thrice => "thrice",
            SoundMode::UntilDismiss => "until",
        }
    }
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            SoundMode::Off => "关闭",
            SoundMode::Once => "单次",
            SoundMode::Twice => "循环 2 次",
            SoundMode::Thrice => "循环 3 次",
            SoundMode::UntilDismiss => "直到消息消失",
        }
    }
    #[allow(dead_code)]
    pub const ALL: [SoundMode; 5] = [
        SoundMode::Off, SoundMode::Once, SoundMode::Twice,
        SoundMode::Thrice, SoundMode::UntilDismiss,
    ];
}

/// 一个 IMAP 邮箱账号（设置页可保存多个，mail.rs 同时监听）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MailAccount {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
}

impl MailAccount {
    pub fn ident(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }
    /// 编码成 `host|port|user|pass`（字段里不允许出现 `|` 和 `;`）
    fn encode(&self) -> String {
        format!("{}|{}|{}|{}", self.host, self.port, self.user, self.pass)
    }
    fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('|').collect();
        if parts.len() != 4 {
            return None;
        }
        let port = parts[1].trim().parse::<u16>().ok()?;
        Some(MailAccount {
            host: parts[0].trim().to_string(),
            port,
            user: parts[2].trim().to_string(),
            pass: parts[3].trim().to_string(),
        })
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

// ---------- 界面透明度 ----------

/// 界面透明度（0.2 ~ 1.0，默认 1.0）：一个值管所有 Orbit 界面——胶囊（含分离
/// 模块条 / 音乐条）/ Dock / 展开面板 / 开始菜单 / 横幅；天气卡片不受影响。
pub fn ui_alpha() -> f32 {
    match UI_ALPHA.load(Ordering::Relaxed) {
        UNLOADED => 1.0,
        bits => f32::from_bits(bits),
    }
}

/// 拖动滑条时的**即时预览**：只改内存 + 通知重绘，不写盘。
///
/// 滑条每移一点就 `save()` 会不停重写 settings.txt（还要跟主线程的文件监视打架），
/// 所以拖动过程只走这里，松手时再 `persist_ui_alpha()` 落盘。
pub fn preview_ui_alpha(v: f32) {
    let v = v.clamp(ALPHA_MIN, ALPHA_MAX);
    if (v - ui_alpha()).abs() < 1e-4 {
        return;
    }
    UI_ALPHA.store(v.to_bits(), Ordering::Relaxed);
    bump();
}

/// 把当前透明度落盘（滑条松手 / 键盘微调后调用）
pub fn persist_ui_alpha() {
    save();
    bump();
}

// ---------- 尺寸 ----------

/// 滑条上的原始值（1.0 = 滑条 100%），夹到量程内
pub fn pill_scale_setting() -> f32 {
    pill_scale_raw()
}

/// 滑条上的原始值（1.0 = 滑条 100%）
pub fn icon_scale_setting() -> f32 {
    icon_scale_raw()
}

fn pill_scale_raw() -> f32 {
    match PILL_SCALE.load(Ordering::Relaxed) {
        UNLOADED => 1.0,
        bits => f32::from_bits(bits).clamp(PILL_SCALE_MIN, PILL_SCALE_MAX),
    }
}

fn icon_scale_raw() -> f32 {
    match ICON_SCALE.load(Ordering::Relaxed) {
        UNLOADED => 1.0,
        bits => f32::from_bits(bits).clamp(ICON_SCALE_MIN, ICON_SCALE_MAX),
    }
}

/// 胶囊（顶部胶囊 + 分离模块条 + 音乐条）的整体缩放，1.0 = 滑条 100%。
/// 实际绘制倍率已经乘上 `PILL_SCALE_BASE`（100% = 老基准的 85%）。
pub fn pill_scale() -> f32 {
    pill_scale_raw() * PILL_SCALE_BASE
}

/// Dock 图标（连同 Dock 高度 / 槽位 / 托盘图标）的缩放，1.0 = 滑条 100%。
/// 实际绘制倍率已经乘上 `ICON_SCALE_BASE`（100% = 老基准的 95%）。
pub fn icon_scale() -> f32 {
    icon_scale_raw() * ICON_SCALE_BASE
}

/// 滑条拖动中的即时预览（只改内存 + 通知重绘，不写盘）
pub fn preview_pill_scale(v: f32) {
    let v = v.clamp(PILL_SCALE_MIN, PILL_SCALE_MAX);
    if (v - pill_scale_raw()).abs() < 1e-4 {
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
    if (v - icon_scale_raw()).abs() < 1e-4 {
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
    if (v - pill_scale_raw()).abs() < 1e-4 {
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

/// 横幅滑条上的原始值（1.0 = 滑条 100%，即当前默认卡片尺寸）
pub fn banner_scale_setting() -> f32 {
    banner_scale_raw()
}

fn banner_scale_raw() -> f32 {
    match BANNER_SCALE.load(Ordering::Relaxed) {
        UNLOADED => 1.0,
        bits => f32::from_bits(bits).clamp(BANNER_SCALE_MIN, BANNER_SCALE_MAX),
    }
}

/// 右下角横幅的整体缩放（卡片宽高 / 字号 / 内边距一起等比变），1.0 = 滑条 100%。
/// 实际绘制倍率已经乘上 `BANNER_SCALE_BASE`（100% = 发布版实际大小）
pub fn banner_scale() -> f32 {
    banner_scale_raw() * BANNER_SCALE_BASE
}

/// 滑条拖动中的即时预览（只改内存 + 通知重绘，不写盘）
pub fn preview_banner_scale(v: f32) {
    let v = v.clamp(BANNER_SCALE_MIN, BANNER_SCALE_MAX);
    if (v - banner_scale_raw()).abs() < 1e-4 {
        return;
    }
    BANNER_SCALE.store(v.to_bits(), Ordering::Relaxed);
    bump();
}

pub fn persist_banner_scale() {
    save();
    bump();
}

/// 一步到位地设值（「重置」按钮用）：改内存 + 落盘 + 通知重绘
pub fn set_banner_scale(v: f32) {
    let v = v.clamp(BANNER_SCALE_MIN, BANNER_SCALE_MAX);
    if (v - banner_scale_raw()).abs() < 1e-4 {
        return;
    }
    BANNER_SCALE.store(v.to_bits(), Ordering::Relaxed);
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

/// 胶囊锚位/形状
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PillAnchor {
    /// 悬浮在顶部下方一点（圆胶囊）
    Float,
    /// 吸顶刘海：贴屏幕顶，顶部两角方形
    Island,
}

impl PillAnchor {
    fn code(self) -> u32 {
        match self {
            PillAnchor::Float => 1,
            PillAnchor::Island => 2,
        }
    }

    fn from_code(c: u32) -> Self {
        match c {
            2 => PillAnchor::Island,
            _ => PillAnchor::Float, // 老值 3 = 吸底，已取消
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "float" | "free" | "悬浮" | "浮动" => Some(PillAnchor::Float),
            "island" | "notch" | "刘海" | "吸顶" => Some(PillAnchor::Island),
            // 老配置里的「吸底」：形状已取消，按悬浮处理（不让整条设置失效）
            "bottom" | "吸底" | "底部" => Some(PillAnchor::Float),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            PillAnchor::Float => "float",
            PillAnchor::Island => "island",
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

/// 高德 key：只从 `settings.txt` 读（源码里不留真 key，避免进 git 历史）
pub fn amap_key() -> String {
    AMAP_KEY.lock().map(|s| s.clone()).unwrap_or_default()
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

#[allow(dead_code)]
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

/// 天气卡片配色方案（0 自动跟壁纸 / 1 深色背景·浅色字 / 2 浅色背景·深色字）
pub fn weather_scheme() -> u32 {
    match WEATHER_SCHEME.load(Ordering::Relaxed) {
        UNLOADED => 0,
        v => v.min(2),
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

/// 歌词提前量（秒）：显示"播放位置 + 提前量"对应的那句，让字幕贴住听感。
/// 默认 1 秒；0 = 不提前；**负值 = 延后**（歌词比听到的晚一点换）。
pub fn lyric_lead() -> f32 {
    match LYRIC_LEAD.load(Ordering::Relaxed) {
        UNLOADED => 1.0,
        bits => f32::from_bits(bits).clamp(-LYRIC_LEAD_MAX, LYRIC_LEAD_MAX),
    }
}

/// 设置歌词提前量（-5 ~ 5 秒，步进 0.1 秒由设置页控制）
pub fn set_lyric_lead(secs: f32) {
    let secs = secs.clamp(-LYRIC_LEAD_MAX, LYRIC_LEAD_MAX);
    if (secs - lyric_lead()).abs() < 1e-3 {
        return;
    }
    LYRIC_LEAD.store(secs.to_bits(), Ordering::Relaxed);
    save();
    bump();
}

// ---------- 圆角 ----------

/// 统一圆角半径（逻辑点）：胶囊 / 音乐条 / 模块条 / Dock 共用。
/// 默认 14 → 圆角矩形；调到条高一半就是原来的完全胶囊形，0 = 直角。
pub fn corner_radius() -> f32 {
    match CORNER.load(Ordering::Relaxed) {
        UNLOADED => CORNER_DEFAULT,
        bits => f32::from_bits(bits).clamp(CORNER_MIN, CORNER_MAX),
    }
}

/// 实际绘制用的半径：不超过条高的一半，再高也是胶囊形
pub fn corner_radius_for(fh: f32) -> f32 {
    corner_radius().min(fh / 2.0).max(0.0)
}

/// 滑条拖动中的即时预览（只改内存 + 通知重绘，不写盘）
pub fn preview_corner_radius(v: f32) {
    let v = v.clamp(CORNER_MIN, CORNER_MAX);
    if (v - corner_radius()).abs() < 1e-4 {
        return;
    }
    CORNER.store(v.to_bits(), Ordering::Relaxed);
    bump();
}

pub fn persist_corner_radius() {
    save();
    bump();
}

/// 一步到位地设值（「重置」按钮用）：改内存 + 落盘 + 通知重绘
pub fn set_corner_radius(v: f32) {
    let v = v.clamp(CORNER_MIN, CORNER_MAX);
    if (v - corner_radius()).abs() < 1e-4 {
        return;
    }
    CORNER.store(v.to_bits(), Ordering::Relaxed);
    save();
    bump();
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

/// 微信新消息是否在屏幕右下角弹会话名+摘要小卡片（只读 UIA）。
/// 默认关（托盘闪烁直接弹通用横幅）；设置页可再开，开后读取线程自动启动。
pub fn wechat_cards() -> bool {
    match WECHAT_CARDS.load(Ordering::Relaxed) {
        UNLOADED => false,
        c => c == 2,
    }
}

/// 鼠标停在 Dock 的微信图标上时，是否弹出「未读会话」预览浮窗（点一条即可直达）。
/// 默认**关**：保留微信自己的悬停小窗；开了才换成 orbit 自绘的多条预览。
pub fn wechat_hover_preview() -> bool {
    match WECHAT_HOVER.load(Ordering::Relaxed) {
        UNLOADED => false,
        c => c == 2,
    }
}

/// 点微信消息横幅时，是否直接切到那个会话（默认**关**）。
/// 关着只把微信窗口唤到前台，不碰当前对话。
/// 注意直达即打开该会话，会把它标为已读。
pub fn wechat_banner_jump() -> bool {
    match WECHAT_JUMP.load(Ordering::Relaxed) {
        UNLOADED => false,
        c => c == 2,
    }
}

/// 微信卡片白名单（标题精确匹配，英文逗号分隔展示/存储）
pub fn wechat_ignore() -> Vec<String> {
    WECHAT_IGNORE.lock().map(|v| v.clone()).unwrap_or_default()
}

/// 设置页输入框用的文本
pub fn wechat_ignore_text() -> String {
    wechat_ignore().join(",")
}

/// 这个会话标题是否在白名单里（不弹卡片）
pub fn is_wechat_ignored(title: &str) -> bool {
    let title = title.trim();
    WECHAT_IGNORE
        .lock()
        .map(|v| v.iter().any(|s| s == title))
        .unwrap_or(false)
}

/// 更新微信卡片白名单；空项自动去掉，标题前后空格 trim
pub fn set_wechat_ignore(list: Vec<String>) {
    let mut clean: Vec<String> = Vec::new();
    for s in list {
        let s = s.trim();
        if !s.is_empty() && !clean.iter().any(|k| k == s) {
            clean.push(s.to_string());
        }
    }
    let changed = WECHAT_IGNORE
        .lock()
        .map(|mut v| {
            if *v == clean {
                false
            } else {
                *v = clean;
                true
            }
        })
        .unwrap_or(false);
    if changed {
        save();
        bump();
    }
}

/// 微信实例热键列表（按实例创建先后对应）
pub fn wechat_hotkeys() -> Vec<String> {
    WECHAT_HOTKEYS.lock().map(|v| v.clone()).unwrap_or_default()
}

/// 设置页输入框用的文本
pub fn wechat_hotkeys_text() -> String {
    wechat_hotkeys().join(",")
}

/// 更新微信热键；空项去掉，保持顺序
pub fn set_wechat_hotkeys(list: Vec<String>) {
    let mut clean: Vec<String> = Vec::new();
    for s in list {
        let s = s.trim().to_string();
        if !s.is_empty() {
            clean.push(s);
        }
    }
    let changed = WECHAT_HOTKEYS
        .lock()
        .map(|mut v| {
            if *v == clean {
                false
            } else {
                *v = clean;
                true
            }
        })
        .unwrap_or(false);
    if changed {
        save();
        bump();
    }
}

/// 微信热键的「按账号」绑定表：(账号快照 id, 热键文本)
pub fn wechat_hotkey_binds() -> Vec<(String, String)> {
    WECHAT_HOTKEY_BIND
        .lock()
        .map(|v| v.clone())
        .unwrap_or_default()
}

/// 某账号绑定的热键（空 = 没绑）。账号用快照 id（= `data/wechat_accounts/<id>/`），
/// 换昵称、换登录顺序都不影响。
pub fn wechat_hotkey_for(id: &str) -> String {
    WECHAT_HOTKEY_BIND
        .lock()
        .ok()
        .and_then(|v| v.iter().find(|(k, _)| k == id).map(|(_, s)| s.clone()))
        .unwrap_or_default()
}

/// 给账号设热键；`spec` 传空 = 解绑
pub fn set_wechat_hotkey_for(id: &str, spec: &str) {
    let id = id.trim();
    if id.is_empty() {
        return;
    }
    let spec = spec.trim().to_string();
    let changed = WECHAT_HOTKEY_BIND
        .lock()
        .map(|mut v| {
            let before = v.clone();
            v.retain(|(k, _)| k != id);
            if !spec.is_empty() {
                v.push((id.to_string(), spec));
            }
            *v != before
        })
        .unwrap_or(false);
    if changed {
        save();
        bump();
    }
}

/// 设置文件里的存法：`账号id=热键,账号id=热键`
pub fn wechat_hotkey_binds_text() -> String {
    wechat_hotkey_binds()
        .into_iter()
        .map(|(k, s)| format!("{k}={s}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// 微信主程序路径（空 = 自动探测）
pub fn wechat_path() -> String {
    WECHAT_PATH.lock().map(|v| v.clone()).unwrap_or_default()
}

/// 设置微信主程序路径
pub fn set_wechat_path(v: &str) {
    let v = v.trim().to_string();
    let changed = WECHAT_PATH.lock().map(|s| s.clone() != v).unwrap_or(false);
    if changed {
        *WECHAT_PATH.lock().unwrap() = v.clone();
        save();
        bump();
    }
}

/// 微信数据目录 xwechat_files（空 = 自动探测）
pub fn wechat_data_dir() -> String {
    WECHAT_DATA_DIR.lock().map(|v| v.clone()).unwrap_or_default()
}

/// 设置微信数据目录
pub fn set_wechat_data_dir(v: &str) {
    let v = v.trim().to_string();
    let changed = WECHAT_DATA_DIR.lock().map(|s| s.clone() != v).unwrap_or(false);
    if changed {
        *WECHAT_DATA_DIR.lock().unwrap() = v.clone();
        save();
        bump();
    }
}

/// 点微信「关闭」是否改成最小化（默认开）
pub fn wechat_close_min() -> bool {
    match WECHAT_CLOSE_MIN.load(Ordering::Relaxed) {
        UNLOADED => true,
        c => c == 2,
    }
}

/// 设置「关闭 = 最小化」。目前只支持手改 `settings.txt`（设置页暂未给开关），
/// 保留 setter 与其它 `set_*` 对称，供以后接 UI 用。
#[allow(dead_code)]
pub fn set_wechat_close_min(on: bool) {
    if wechat_close_min() == on {
        return;
    }
    WECHAT_CLOSE_MIN.store(bool_code(on), Ordering::Relaxed);
    save();
    bump();
}

/// 右下角横幅（微信消息卡片等）每条自动关闭的秒数；0 = 不自动关闭。
/// 原子内部存「秒数 + 1」：UNLOADED 哨兵是 0，直接存秒数会让「0 秒」
/// 撞上哨兵被当成未加载、永远读回默认 6（实测 bug）。
pub fn banner_secs() -> u32 {
    match BANNER_SECS.load(Ordering::Relaxed) {
        UNLOADED => 6,
        n => n.saturating_sub(1),
    }
}

/// 设置横幅停留秒数（设置页输入框；0 = 不自动关闭，上限 3600）。
/// 这一项**立即落盘**，不走「点应用才写盘」的草稿机制：文本框失焦/窗口失活
/// 的时序会让草稿先丢弃，实测「改了又变回默认」。
#[allow(dead_code)]
pub fn set_banner_secs(secs: u32) {
    let secs = secs.min(3600);
    if secs == banner_secs() {
        return;
    }
    BANNER_SECS.store(secs + 1, Ordering::Relaxed);
    let was = EDITING.swap(false, Ordering::Relaxed);
    save();
    if was {
        EDITING.store(true, Ordering::Relaxed);
    }
    bump();
}

// ---------- 界面字体 ----------

/// 是否使用 MiSans（随包分发、磁盘加载；文件缺失时回落系统微软雅黑等）
pub fn use_misans() -> bool {
    match FONT.load(Ordering::Relaxed) {
        UNLOADED => true,
        2 => false,
        _ => true,
    }
}

/// 切换界面字体。改变会让所有窗口在下一个渲染帧重建字体图集。
pub fn set_use_misans(on: bool) {
    let code = if on { 1 } else { 2 };
    if FONT.load(Ordering::Relaxed) == code {
        return;
    }
    FONT.store(code, Ordering::Relaxed);
    save();
    bump();
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

// ---------- 托盘图标排序 ----------

/// 用户自定义的托盘图标顺序（`tray::TrayItem::key` 列表）。空 = 没自定义
pub fn tray_order() -> Vec<String> {
    TRAY_ORDER.lock().map(|v| v.clone()).unwrap_or_default()
}

/// 写回自定义顺序（设置页 ▲▼ 排序；传空 = 恢复默认排序）。
/// 键去重去空，顺序即 Dock 上托盘图标的显示顺序。
pub fn set_tray_order(order: Vec<String>) {
    let mut list: Vec<String> = Vec::new();
    for k in order {
        let k = k.trim().to_string();
        if !k.is_empty() && !list.iter().any(|o| *o == k) {
            list.push(k);
        }
    }
    let changed = TRAY_ORDER
        .lock()
        .map(|mut v| {
            let same = *v == list;
            if !same {
                *v = list;
            }
            !same
        })
        .unwrap_or(false);
    if changed {
        save();
        bump();
    }
}

// ---------- 图标指定 ----------

/// 图标指定表：`(键, 取图标的 exe/.lnk)`
pub fn icon_overrides() -> Vec<(String, String)> {
    ICON_OVER.lock().map(|v| v.clone()).unwrap_or_default()
}

/// 查某个键（托盘图标键 / `app:<exe>`）有没有被指定图标
pub fn icon_override(key: &str) -> Option<String> {
    ICON_OVER
        .lock()
        .ok()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, p)| p.clone())
}

/// 指定 / 取消指定某个键的图标来源（`None` = 恢复默认）
pub fn set_icon_override(key: &str, path: Option<&str>) {
    let mut changed = false;
    if let Ok(mut v) = ICON_OVER.lock() {
        let pos = v.iter().position(|(k, _)| k.eq_ignore_ascii_case(key));
        match (path.map(str::trim).filter(|p| !p.is_empty()), pos) {
            (Some(p), Some(i)) => {
                if !v[i].1.eq_ignore_ascii_case(p) {
                    v[i].1 = p.to_string();
                    changed = true;
                }
            }
            (Some(p), None) => {
                v.push((key.to_string(), p.to_string()));
                changed = true;
            }
            (None, Some(i)) => {
                v.remove(i);
                changed = true;
            }
            (None, None) => {}
        }
    }
    if changed {
        save();
        bump();
    }
}

/// 设置版本号：任何一项变化都 +1。各窗口记下自己见过的版本，
/// 对不上就重绘——比"谁先看到 mtime 变化"更可靠（设置页面改值不经过 mtime 轮询）。
pub fn version() -> u64 {
    VERSION.load(Ordering::Relaxed)
}

// ---------- 编辑会话（设置页「应用」按钮） ----------

/// 设置页打开时进入编辑会话：之后所有 `set_*` 只改内存、不写盘不 bump。
/// 点「应用」→ `commit_edit()` 一次落盘；点「还原」/关窗 → `discard_edit()` 从文件恢复。
pub fn begin_edit() {
    EDITING.store(true, Ordering::Relaxed);
}

/// 把编辑会话期间的所有改动一次性落盘，并结束会话。
pub fn commit_edit() {
    EDITING.store(false, Ordering::Relaxed);
    save();
    bump();
    // 主动通知胶囊 / Dock 立即重绘，不必等各自 500ms 的轮询（主题/模块切换的顿挫感来源）
    crate::appmsg::notify_applied();
}

/// 丢弃编辑会话期间的改动：从 settings.txt 重读恢复内存，并结束会话。
pub fn discard_edit() {
    EDITING.store(false, Ordering::Relaxed);
    load();
    bump();
    // 还原同样要立刻刷新回旧值
    crate::appmsg::notify_applied();
}

/// 当前是否在编辑会话中（设置页据此决定「应用/还原」按钮显隐与高亮）。
pub fn editing() -> bool {
    EDITING.load(Ordering::Relaxed)
}

// ---------- Dock 固定图标 ----------

/// Dock 固定图标列表（exe / .lnk 全路径，按显示顺序）。
///
/// `None` = 用户还没在设置页改过 → 调用方回落到系统任务栏的钉选（保持老观感）；
/// `Some(vec![])` = 用户明确清空了固定列表（一个都不钉）。
pub fn dock_pins() -> Option<Vec<String>> {
    DOCK_PINS.lock().ok().and_then(|v| v.clone())
}

/// 覆盖整个固定列表（设置页的增 / 删 / 清空都走这里）。
///
/// 注意：第一次改动必须传"完整的期望列表"——`dock_pins()` 为 `None` 时界面上显示的
/// 其实是系统任务栏的钉选，调用方要先用 `dock::effective_pins()` 取到实际列表再增删。
pub fn set_dock_pins(list: Vec<String>) {
    if let Ok(mut v) = DOCK_PINS.lock() {
        *v = Some(list);
    }
    save();
    bump();
}

// ---------- 开始菜单 / 系统 ----------

/// 开始菜单快捷方式默认项（用户没改过时的唯一一条）
fn default_start_pins() -> Vec<(String, String)> {
    // 默认带一个「管理员权限 CMD」：target 前缀 `@admin:` 表示用 ShellExecute(runas) 提权
    vec![(
        "命令提示符(管理员)".to_string(),
        "@admin:C:\\Windows\\System32\\cmd.exe".to_string(),
    )]
}

/// 开始菜单快捷方式列表（设置页没改过 → 默认「命令提示符」）
pub fn start_pins() -> Vec<(String, String)> {
    match START_PINS.lock().ok().and_then(|v| v.clone()) {
        Some(list) => list,
        None => default_start_pins(),
    }
}

/// 用户自定义过的开始菜单快捷方式（None = 还没改过；`Some(vec![])` = 清空了）
pub fn start_pins_custom() -> Option<Vec<(String, String)>> {
    START_PINS.lock().ok().and_then(|v| v.clone())
}

/// 覆盖开始菜单快捷方式列表（设置页增删走这里）
pub fn set_start_pins(list: Vec<(String, String)>) {
    let mut clean: Vec<(String, String)> = Vec::new();
    for (name, target) in list {
        let (name, target) = (name.trim().to_string(), target.trim().to_string());
        if name.is_empty() || target.is_empty() {
            continue;
        }
        if !clean.iter().any(|(_, p)| p.eq_ignore_ascii_case(&target)) {
            clean.push((name, target));
        }
    }
    let changed = START_PINS
        .lock()
        .map(|mut v| {
            let diff = v.as_ref() != Some(&clean);
            if diff {
                *v = Some(clean);
            }
            diff
        })
        .unwrap_or(false);
    if changed {
        save();
        bump();
    }
}

/// 任务管理器 exe 的完整路径（设置页「快速添加 → 任务管理器」用）。
///
/// 系统里没有「任务管理器」的开始菜单快捷方式，想加到 Orbit 开始菜单只能自己填路径。
/// 32 位进程看到的 `System32` 会被重定向到 `SysWOW64`（那里没有 Taskmgr.exe），
/// 所以再退一步试 `Sysnative`；都找不到就返回 `None`，设置页据此禁用入口。
pub fn taskmgr_path() -> Option<PathBuf> {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
    ["System32", "Sysnative"]
        .iter()
        .map(|sub| PathBuf::from(&windir).join(sub).join("Taskmgr.exe"))
        .find(|p| p.is_file())
}

/// 开始菜单快捷方式最多显示几行（每行数量可配，行数固定两行）
pub const START_MAX_ROWS: usize = 2;

/// 开始菜单最多能显示几条快捷方式（每行数量 × 2 行）。
/// 设置页按这个上限禁用「添加」，`startmenu::max_tiles` 也走这里，两边不会算岔。
pub fn start_max_pins() -> usize {
    start_cols() * START_MAX_ROWS
}

/// 开机启动（写注册表 HKCU\...\Run；注册表的实际增删见 guard.rs，
/// 启动时 sync_autostart() 会按这里的值对齐注册表）
pub fn autostart() -> bool {
    match AUTOSTART.load(Ordering::Relaxed) {
        UNLOADED => true,
        c => c == 2,
    }
}

pub fn set_autostart(on: bool) {
    if autostart() == on {
        return;
    }
    AUTOSTART.store(bool_code(on), Ordering::Relaxed);
    save();
    bump();
}

/// 退出守护（默认开；开关在下次启动 orbit 时生效——守护子进程随主进程拉起）
pub fn guardian() -> bool {
    match GUARDIAN.load(Ordering::Relaxed) {
        UNLOADED => true,
        c => c == 2,
    }
}

pub fn set_guardian(on: bool) {
    if guardian() == on {
        return;
    }
    GUARDIAN.store(bool_code(on), Ordering::Relaxed);
    save();
    bump();
}

/// 伪造托盘点击/悬停（traymsg 回调消息投递）。默认开；关掉做透明转发 A/B。
pub fn tray_fake_input() -> bool {
    match TRAY_FAKE_INPUT.load(Ordering::Relaxed) {
        UNLOADED => cfg::TRAY_FAKE_INPUT,
        c => c == 2,
    }
}

#[allow(dead_code)]
pub fn set_tray_fake_input(on: bool) {
    if tray_fake_input() == on {
        return;
    }
    TRAY_FAKE_INPUT.store(bool_code(on), Ordering::Relaxed);
    save();
    bump();
}

/// Dock 上的内建「系统控制」按钮。默认隐藏；设置页 → Dock 里打开。
pub fn sys_panel() -> bool {
    match SYS_PANEL.load(Ordering::Relaxed) {
        UNLOADED => false,
        c => c == 2,
    }
}

pub fn set_sys_panel(on: bool) {
    if sys_panel() == on {
        return;
    }
    SYS_PANEL.store(bool_code(on), Ordering::Relaxed);
    save();
    bump();
}

// ---------- 通知通道（每个消息源独立）----------

/// 微信消息走哪条通知通道（默认 Orbit 横幅）
pub fn notify_wechat() -> NotifyChannel {
    NotifyChannel::from_code(match NOTIFY_WECHAT.load(Ordering::Relaxed) {
        UNLOADED => NotifyChannel::Orbit.code(),
        c => c,
    })
}
pub fn set_notify_wechat(c: NotifyChannel) {
    if notify_wechat() == c { return; }
    NOTIFY_WECHAT.store(c.code(), Ordering::Relaxed);
    save(); bump();
}

/// 托盘图标消息走哪条通知通道（默认 Orbit 横幅）
pub fn notify_tray() -> NotifyChannel {
    NotifyChannel::from_code(match NOTIFY_TRAY.load(Ordering::Relaxed) {
        UNLOADED => NotifyChannel::Orbit.code(),
        c => c,
    })
}
pub fn set_notify_tray(c: NotifyChannel) {
    if notify_tray() == c { return; }
    NOTIFY_TRAY.store(c.code(), Ordering::Relaxed);
    save(); bump();
}

/// 邮件新消息走哪条通知通道（默认 Orbit 横幅）
pub fn notify_mail() -> NotifyChannel {
    NotifyChannel::from_code(match NOTIFY_MAIL.load(Ordering::Relaxed) {
        UNLOADED => NotifyChannel::Orbit.code(),
        c => c,
    })
}
pub fn set_notify_mail(c: NotifyChannel) {
    if notify_mail() == c { return; }
    NOTIFY_MAIL.store(c.code(), Ordering::Relaxed);
    save(); bump();
}

/// 剪切板新条目走哪条通知通道（默认 Orbit 横幅）
pub fn notify_clipboard() -> NotifyChannel {
    NotifyChannel::from_code(match NOTIFY_CLIPBOARD.load(Ordering::Relaxed) {
        UNLOADED => NotifyChannel::Orbit.code(),
        c => c,
    })
}
pub fn set_notify_clipboard(c: NotifyChannel) {
    if notify_clipboard() == c { return; }
    NOTIFY_CLIPBOARD.store(c.code(), Ordering::Relaxed);
    save(); bump();
}

// ---------- 各来源横幅时长（0 = 常驻）----------

fn banner_secs_from(a: &AtomicU32, default_secs: u32) -> u32 {
    match a.load(Ordering::Relaxed) {
        UNLOADED => default_secs,
        v => v.saturating_sub(1).min(3600),
    }
}
fn set_banner_secs_at(a: &AtomicU32, secs: u32) {
    a.store(secs.min(3600) + 1, Ordering::Relaxed);
    save(); bump();
}

pub fn banner_secs_wechat() -> u32 { banner_secs_from(&BANNER_WECHAT_SECS, 6) }
pub fn set_banner_secs_wechat(s: u32) {
    if banner_secs_wechat() == s { return; }
    set_banner_secs_at(&BANNER_WECHAT_SECS, s);
}
pub fn banner_secs_mail() -> u32 { banner_secs_from(&BANNER_MAIL_SECS, 6) }
pub fn set_banner_secs_mail(s: u32) {
    if banner_secs_mail() == s { return; }
    set_banner_secs_at(&BANNER_MAIL_SECS, s);
}
pub fn banner_secs_tray() -> u32 { banner_secs_from(&BANNER_TRAY_SECS, 6) }
pub fn set_banner_secs_tray(s: u32) {
    if banner_secs_tray() == s { return; }
    set_banner_secs_at(&BANNER_TRAY_SECS, s);
}
pub fn banner_secs_clipboard() -> u32 { banner_secs_from(&BANNER_CLIPBOARD_SECS, 6) }
pub fn set_banner_secs_clipboard(s: u32) {
    if banner_secs_clipboard() == s { return; }
    set_banner_secs_at(&BANNER_CLIPBOARD_SECS, s);
}

/// 按通知来源取横幅时长（供 banner.rs 调用）
pub fn banner_secs_for(src: &str) -> u32 {
    match src.to_lowercase().as_str() {
        "wechat" | "微信" => banner_secs_wechat(),
        "mail" | "邮件" => banner_secs_mail(),
        "tray" | "托盘" => banner_secs_tray(),
        "clipboard" | "剪切板" | "剪贴板" => banner_secs_clipboard(),
        _ => banner_secs(),
    }
}

/// 按通知来源取通道（供 banner.rs 分发）
pub fn notify_channel_for(src: &str) -> NotifyChannel {
    match src.to_lowercase().as_str() {
        "wechat" | "微信" => notify_wechat(),
        "mail" | "邮件" => notify_mail(),
        "tray" | "托盘" => notify_tray(),
        "clipboard" | "剪切板" | "剪贴板" => notify_clipboard(),
        _ => NotifyChannel::Orbit,
    }
}

// ---------- 接管系统通知（通知中心）----------

/// 接管 Windows 通知中心：开着时 `notifycenter` 线程监听全系统 Toast，
/// 改在 Orbit 横幅里展示，并从通知中心撤掉（见 `notifycenter.rs`）。
pub fn notify_takeover() -> bool {
    match NOTIFY_TAKEOVER.load(Ordering::Relaxed) {
        UNLOADED => false,
        c => c == 2,
    }
}
pub fn set_notify_takeover(on: bool) {
    if notify_takeover() == on {
        return;
    }
    NOTIFY_TAKEOVER.store(bool_code(on), Ordering::Relaxed);
    save();
    bump();
}

// ---------- 提示音模式 ----------

pub fn sound_mode() -> SoundMode {
    SoundMode::from_code(match SOUND_MODE.load(Ordering::Relaxed) {
        UNLOADED => SoundMode::Once.code(),
        c => c,
    })
}
pub fn set_sound_mode(m: SoundMode) {
    if sound_mode() == m { return; }
    SOUND_MODE.store(m.code(), Ordering::Relaxed);
    save(); bump();
}

// ---------- 邮件多账号 ----------

pub fn mail_accounts() -> Vec<MailAccount> {
    MAIL_ACCOUNTS.lock().map(|v| v.clone()).unwrap_or_default()
}

/// 有效账号列表：多账号非空就用它，否则回落到单账号 imap_*（兼容旧配置）
pub fn effective_mail_accounts() -> Vec<MailAccount> {
    let multi = mail_accounts();
    if !multi.is_empty() {
        return multi;
    }
    let (h, p, u, w) = mail_overrides();
    if let (Some(host), Some(user), Some(pass)) = (h, u, w) {
        return vec![MailAccount {
            host,
            port: p.unwrap_or(cfg::MAIL_PORT),
            user,
            pass,
        }];
    }
    Vec::new()
}

pub fn set_mail_accounts(list: Vec<MailAccount>) {
    let mut clean: Vec<MailAccount> = Vec::new();
    for a in list {
        if a.host.trim().is_empty() || a.user.trim().is_empty() {
            continue;
        }
        if !clean.iter().any(|x| x.ident() == a.ident()) {
            clean.push(a);
        }
    }
    let changed = MAIL_ACCOUNTS
        .lock()
        .map(|mut v| {
            let diff = *v != clean;
            if diff { *v = clean; }
            diff
        })
        .unwrap_or(false);
    if changed { save(); bump(); }
}

pub fn mail_verified() -> Vec<String> {
    MAIL_VERIFIED.lock().map(|v| v.clone()).unwrap_or_default()
}

pub fn is_mail_verified(ident: &str) -> bool {
    MAIL_VERIFIED
        .lock()
        .map(|v| v.iter().any(|s| s.eq_ignore_ascii_case(ident)))
        .unwrap_or(false)
}

/// 标记账号连通状态；连通成功加入、失败移除
pub fn set_mail_verified(ident: &str, ok: bool) {
    let changed = MAIL_VERIFIED
        .lock()
        .map(|mut v| {
            let exists = v.iter().position(|s| s.eq_ignore_ascii_case(ident));
            match (ok, exists) {
                (true, None) => { v.push(ident.to_string()); true }
                (false, Some(i)) => { v.remove(i); true }
                _ => false,
            }
        })
        .unwrap_or(false);
    if changed { save(); bump(); }
}

// ---------- 剪切板常驻文字 ----------

pub fn clipboard_pinned_text() -> String {
    CLIPBOARD_PINNED_TEXT.lock().map(|s| s.clone()).unwrap_or_default()
}
pub fn set_clipboard_pinned_text(s: String) {
    let changed = CLIPBOARD_PINNED_TEXT
        .lock()
        .map(|mut v| {
            let diff = *v != s;
            if diff { *v = s; }
            diff
        })
        .unwrap_or(false);
    if changed { save(); bump(); }
}

// ---------- 开始菜单：Win 键接管 / 每行列数 ----------

pub fn start_hook() -> bool {
    match START_HOOK.load(Ordering::Relaxed) {
        UNLOADED => true,
        c => c == 2,
    }
}
pub fn set_start_hook(on: bool) {
    if start_hook() == on { return; }
    START_HOOK.store(bool_code(on), Ordering::Relaxed);
    save(); bump();
}

pub fn start_cols() -> usize {
    match START_COLS.load(Ordering::Relaxed) {
        UNLOADED => 4,
        c => (c as usize).clamp(1, 8),
    }
}
pub fn set_start_cols(n: usize) {
    let n = n.clamp(1, 8);
    if start_cols() == n { return; }
    START_COLS.store(n as u32, Ordering::Relaxed);
    save(); bump();
}

fn bump() {
    // 编辑会话中不 bump：设置页还没点「应用」，主界面不应重绘
    if EDITING.load(Ordering::Relaxed) {
        return;
    }
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

/// 天气卡片配色方案（设置页三档 seg 用）
pub fn set_weather_scheme(v: u32) {
    let v = v.min(2);
    if v == weather_scheme() {
        return;
    }
    WEATHER_SCHEME.store(v, Ordering::Relaxed);
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
}pub fn set_lyric_position(p: LyricPos) {
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

/// 开关微信消息卡片（设置页）
pub fn set_wechat_cards(on: bool) {
    if on == wechat_cards() {
        return;
    }
    WECHAT_CARDS.store(bool_code(on), Ordering::Relaxed);
    save();
    bump();
}

/// 开关微信托盘悬停预览浮窗（设置页）
pub fn set_wechat_hover_preview(on: bool) {
    if on == wechat_hover_preview() {
        return;
    }
    WECHAT_HOVER.store(bool_code(on), Ordering::Relaxed);
    save();
    bump();
}

/// 开关「点横幅直达会话」（设置页）
pub fn set_wechat_banner_jump(on: bool) {
    if on == wechat_banner_jump() {
        return;
    }
    WECHAT_JUMP.store(bool_code(on), Ordering::Relaxed);
    save();
    bump();
}

/// 按当前值更新设置文件：**只改我们管的键的值**，注释、空行、用户自己加的
/// 其它键都原样保留（手改过的文件不会被洗掉）；缺的键补在末尾。
/// 写完把 mtime 同步掉，免得下一轮轮询又当成"外部改动"重载一次。
fn save() {
    // 编辑会话中不落盘：改动暂存在内存，点「应用」才 commit_edit() 统一写文件
    if EDITING.load(Ordering::Relaxed) {
        return;
    }
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
    let mut vals: Vec<(&str, String)> = vec![
        ("clock_format", fmt),
        ("module_order", order),
        ("weather_mode", mode("weather")),
        ("mail_mode", mode("mail")),
        ("clipboard_mode", mode("clipboard")),
        ("alpha", trim_num(ui_alpha())),
        ("pill_scale", trim_num(pill_scale_raw())),
        ("icon_scale", trim_num(icon_scale_raw())),
        ("banner_scale", trim_num(banner_scale_raw())),
        ("corner_radius", trim_num(corner_radius())),
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
        ("weather_scheme", weather_scheme().to_string()),
        ("lyric_position", lyric_pos_str(lyric_position()).to_string()),
        ("lyric_size", trim_num(lyric_size())),
        ("lyric_lead", trim_num(lyric_lead())),
        ("show_tray", on_off(show_tray()).to_string()),
        ("show_desktop_btn", on_off(show_desktop_btn()).to_string()),
        ("wechat_sound", on_off(wechat_sound()).to_string()),
        ("wechat_cards", on_off(wechat_cards()).to_string()),
        ("wechat_hover_preview", on_off(wechat_hover_preview()).to_string()),
        ("wechat_banner_jump", on_off(wechat_banner_jump()).to_string()),
        ("wechat_ignore", wechat_ignore().join(",")),
        ("wechat_hotkeys", wechat_hotkeys().join(",")),
        ("wechat_hotkey_bind", wechat_hotkey_binds_text()),
        ("wechat_path", wechat_path()),
        ("wechat_data_dir", wechat_data_dir()),
        ("wechat_close_min", on_off(wechat_close_min()).to_string()),
        ("banner_secs", banner_secs().to_string()),
        ("font", if use_misans() { "misans" } else { "system" }.to_string()),
        ("hide_tray", hidden_tray().join(";")),
        ("tray_order", tray_order().join(";")),
    ];
    // 固定列表按需写：用户还没改过（None）就不落键，免得把系统任务栏钉选"写空"
    if let Some(pins) = dock_pins() {
        vals.push(("dock_pins", pins.join(";")));
    }
    // 开始菜单快捷方式同上：没改过不落键（改过才覆盖，删掉该键即恢复默认）
    if let Some(pins) = start_pins_custom() {
        let s: Vec<String> = pins.iter().map(|(n, p)| format!("{n}|{p}")).collect();
        vals.push(("start_pins", s.join(";")));
    }
    vals.push(("autostart", on_off(autostart()).to_string()));
    vals.push(("guardian", on_off(guardian()).to_string()));
    vals.push(("tray_fake_input", on_off(tray_fake_input()).to_string()));
    // Dock 内建「系统控制」按钮（默认隐藏，开着才写 1 也行，统一落键便于设置页回显）
    vals.push(("sys_panel", on_off(sys_panel()).to_string()));
    // 通知通道（每个来源独立）
    vals.push(("notify_wechat", notify_wechat().as_str().to_string()));
    vals.push(("notify_tray", notify_tray().as_str().to_string()));
    vals.push(("notify_mail", notify_mail().as_str().to_string()));
    vals.push(("notify_clipboard", notify_clipboard().as_str().to_string()));
    // 各来源横幅时长（0 = 常驻）
    vals.push(("banner_wechat_secs", banner_secs_wechat().to_string()));
    vals.push(("banner_mail_secs", banner_secs_mail().to_string()));
    vals.push(("banner_tray_secs", banner_secs_tray().to_string()));
    vals.push(("banner_clipboard_secs", banner_secs_clipboard().to_string()));
    vals.push(("sound_mode", sound_mode().as_str().to_string()));
    // 接管系统通知中心
    vals.push(("notify_takeover", on_off(notify_takeover()).to_string()));
    // 邮件多账号（`;` 分隔账号，每个 `host|port|user|pass`）
    let accounts: Vec<String> = mail_accounts().iter().map(|a| a.encode()).collect();
    vals.push(("mail_accounts", accounts.join(";")));
    vals.push(("mail_verified", mail_verified().join(";")));
    vals.push(("clipboard_pinned", clipboard_pinned_text()));
    vals.push(("start_hook", on_off(start_hook()).to_string()));
    vals.push(("start_cols", start_cols().to_string()));
    let overs: Vec<String> = icon_overrides()
        .into_iter()
        .map(|(k, p)| format!("{k}|{p}"))
        .collect();
    vals.push(("icon_over", overs.join(";")));
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
    alpha: f32,
    pill_scale: f32,
    icon_scale: f32,
    banner_scale: f32,
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
    wechat_cards: bool,
    wechat_hover_preview: bool,
    wechat_banner_jump: bool,
    wechat_ignore: Vec<String>,
    /// 热键两项也要进快照：否则只改热键（手改文件 / 设置页行内填）时快照没差异，
    /// 主进程不会重装热键，得重启才生效
    wechat_hotkeys: Vec<String>,
    wechat_hotkey_bind: Vec<(String, String)>,
    use_misans: bool,
    hidden_tray: Vec<String>,
    start_pins: Option<Vec<(String, String)>>,
    autostart: bool,
    guardian: bool,
    notify_wechat: NotifyChannel,
    notify_tray: NotifyChannel,
    notify_mail: NotifyChannel,
    notify_clipboard: NotifyChannel,
    banner_wechat_secs: u32,
    banner_mail_secs: u32,
    banner_tray_secs: u32,
    banner_clipboard_secs: u32,
    sound_mode: SoundMode,
    notify_takeover: bool,
    mail_accounts: Vec<MailAccount>,
    mail_verified: Vec<String>,
    clipboard_pinned: String,
    start_hook: bool,
    start_cols: usize,
}

fn snapshot() -> Snap {
    Snap {
        clock_format: clock_format(),
        module_order: module_order(),
        weather_mode: module_mode("weather"),
        mail_mode: module_mode("mail"),
        clipboard_mode: module_mode("clipboard"),
        alpha: ui_alpha(),
        pill_scale: pill_scale(),
        icon_scale: icon_scale(),
        banner_scale: banner_scale(),
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
        wechat_cards: wechat_cards(),
        wechat_hover_preview: wechat_hover_preview(),
        wechat_banner_jump: wechat_banner_jump(),
        wechat_ignore: wechat_ignore(),
        wechat_hotkeys: wechat_hotkeys(),
        wechat_hotkey_bind: wechat_hotkey_binds(),
        use_misans: use_misans(),
        hidden_tray: hidden_tray(),
        start_pins: start_pins_custom(),
        autostart: autostart(),
        guardian: guardian(),
        notify_wechat: notify_wechat(),
        notify_tray: notify_tray(),
        notify_mail: notify_mail(),
        notify_clipboard: notify_clipboard(),
        banner_wechat_secs: banner_secs_wechat(),
        banner_mail_secs: banner_secs_mail(),
        banner_tray_secs: banner_secs_tray(),
        banner_clipboard_secs: banner_secs_clipboard(),
        sound_mode: sound_mode(),
        notify_takeover: notify_takeover(),
        mail_accounts: mail_accounts(),
        mail_verified: mail_verified(),
        clipboard_pinned: clipboard_pinned_text(),
        start_hook: start_hook(),
        start_cols: start_cols(),
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
# 界面透明度（0.2 ~ 1.0）：胶囊（含分离模块条 / 音乐条）/ Dock / 展开面板 /
# 开始菜单 / 横幅一起调，天气卡片不受影响；100% 时底色强制实底
alpha=1.0
# 胶囊整体大小 / Dock 图标大小（1.0 = 滑条 100%，0.8 ~ 1.35；设置页里有滑条和「重置」）
pill_scale=1
icon_scale=1
# 右下角横幅整体大小（1.0 = 默认卡片尺寸，0.8 ~ 1.5；设置页有滑条和「重置」，拖动时右下角弹预览卡）
banner_scale=1
# 统一圆角半径（逻辑点，0 = 直角，默认 14 = 圆角矩形）：
# 顶部胶囊 / 音乐条 / 模块条 / Dock 共用，调到条高一半就是完全胶囊形
corner_radius=14
# 外观主题（每套都有深色 + 浅色）：calm_light/calm_dark（平静极简）/
# glass2_light/glass2_dark（玻璃拟态 2.0）/ m3_light/m3_dark（Material 3）
theme=calm_light
# 胶囊位置形状：float（悬浮顶部，默认）/ island（吸顶刘海）
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
# 天气卡片配色：0 自动跟壁纸（默认）/ 1 深色背景（浅色字）/ 2 浅色背景（深色字）
weather_scheme=0
# 音乐条位置：right（胶囊右侧，默认）/ left（左侧）/ bottom（下方）
lyric_position=right
# 歌词字号
lyric_size=20
# 底部 Dock：show_tray / show_desktop_btn 仍可手改（1 = 显示，0 = 隐藏）；
# 设置页里「返回桌面」在托盘图标列表的第一格开关，show_tray 不再提供开关（逐个隐藏用 hide_tray）
show_tray=1
show_desktop_btn=1
# 微信托盘图标开始闪烁（新消息）时播放内置提示音（1 = 开，默认；0 = 关）。
# 想换音效：直接用同名文件覆盖 data/wechat_notify.mp3 即可
wechat_sound=1
# 微信消息卡片（UIA 读会话名+摘要，1 = 开；0 = 关，默认）。
# 关闭时托盘闪烁直接弹通用横幅；开启后只读微信聊天列表，不改动微信本体
wechat_cards=0
# 微信卡片白名单：标题命中就不弹卡片（分号分隔；默认「折叠的聊天」）
wechat_ignore=折叠的聊天
# 界面字体：misans = MiSans（随包分发，默认）；system = 系统字体（微软雅黑等）
font=misans
# Dock 上要单独隐藏的托盘图标（分号分隔，键由设置页生成；清空 = 全部显示）
hide_tray=
# 托盘图标自定义排序（稳定键列表，分号分隔，设置页 ▲▼ 排序写回；清空 = 默认顺序）
tray_order=
# 开始菜单（Win 键弹出）快捷方式：`名称|程序路径`，分号分隔；删掉本行恢复默认「命令提示符(管理员)」
# 路径前缀 @admin: 表示以管理员权限启动（ShellExecute runas）
start_pins=命令提示符(管理员)|@admin:C:\\Windows\\System32\\cmd.exe
# 开机启动：写注册表 HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run（1 = 开，默认）
autostart=1
# 退出守护：orbit 被强杀（任务栏没恢复）时由守护进程重启 explorer 恢复任务栏（1 = 开，默认）
guardian=1
# 伪造托盘点击/悬停（向目标窗口投它注册时的回调消息）。默认开；
# 关掉后完全交给已转发给 Explorer 的托盘处理，用于透明转发 A/B 测试
tray_fake_input=1
# Dock 内建「系统控制」按钮（音量/亮度/WiFi/蓝牙，Orbit 图标）。默认隐藏，1 = 显示
sys_panel=0
# ===== 通知（每个消息源独立选择通道）=====
# 通道：off=不通知 / orbit=Orbit 横幅 / windows=Windows 系统通知
notify_wechat=orbit
notify_tray=orbit
notify_mail=orbit
notify_clipboard=orbit
# 各来源横幅停留秒数（0 = 不消失，常驻）
banner_wechat_secs=6
banner_mail_secs=6
banner_tray_secs=6
banner_clipboard_secs=6
# 提示音模式：off=关 / once=单次 / twice=循环2次 / thrice=循环3次 / until=直到消息消失
# （旧的 wechat_sound 仅作兼容，提示音实际由 sound_mode 决定）
sound_mode=once
# 接管 Windows 通知中心（1 = 开）：系统通知改在 Orbit 横幅里显示，并从通知中心撤掉；
# 首次开启会向系统申请「通知监听」授权（未打包 exe 可能被拒，被拒时只在日志里提示）
notify_takeover=0
# ===== 邮件多账号（同时监听）=====
# 每个账号 host|port|user|pass，账号间用 ; 分隔；非空时优先于上面的 imap_* 单账号
mail_accounts=
# 已测试连通的账号（user@host，分号分隔），设置页标记用，避免每次重测
mail_verified=
# ===== 剪切板 =====
# 剪切板面板常驻置顶文字（固定显示一条）
clipboard_pinned=
# ===== 开始菜单 =====
# Win 键接管开始菜单（1 = 接管，默认；0 = 交还系统）
start_hook=1
# 每行显示的快捷方式个数（1 ~ 8，默认 4）
start_cols=4
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
    // 键是否已存在按行首 `key=` 判断：不能用 `raw.contains(key)`——新键 `alpha` 是
    // 老键 `pill_alpha` / `dock_alpha` 的子串，contains 会误判成"已经有这个键"
    let has = |k: &str| {
        let pat = format!("{k}=");
        raw.lines().any(|l| l.trim_start().starts_with(pat.as_str()))
    };
    // `alpha` 这个新键要特殊对待：老文件里只有 pill_alpha / dock_alpha，而 load 里
    // 新键优先——补一行 `alpha=1.0` 会把两个旧值迁移出来的结果整个盖掉。
    // 只要文件里还有老键，就不补 alpha（等 save() 改写时再落键）。
    let legacy_alpha = has("pill_alpha") || has("dock_alpha");
    for (key, line) in [
        ("clock_format", "# 顶部胶囊时钟格式串（strftime 风格，占位符见 docs）\nclock_format=%H:%M %m月%d日 %a\n"),
        ("module_order", "# 顶部胶囊模块排列（拖动排序写回）\nmodule_order=weather,mail,clipboard\n"),
        ("weather_mode", "# 模块摆哪：pill / left / right / off\nweather_mode=left\nmail_mode=right\nclipboard_mode=right\n"),
        (
            "alpha",
            "# 界面透明度（0.2 ~ 1.0）：胶囊 / 模块条 / 音乐条 / Dock / 面板 / 开始菜单 / 横幅共用\n\
             # 天气卡片不受影响；100% 时底色强制实底。老键 pill_alpha / dock_alpha 仍兼容读取\nalpha=1.0\n",
        ),
        (
            "pill_scale",
            "# 胶囊 / Dock 图标大小（1.0 = 滑条 100%，0.8 ~ 1.35；设置页有滑条和「重置」）\n\
             pill_scale=1\nicon_scale=1\n",
        ),
        ("banner_scale", "# 右下角横幅整体大小（1.0 = 默认，0.8 ~ 1.5；设置页有滑条和「重置」）\nbanner_scale=1\n"),
        (
            "corner_radius",
            "# 统一圆角半径（逻辑点，0 = 直角，默认 14 = 圆角矩形）：\n\
             # 顶部胶囊 / 音乐条 / 模块条 / Dock 共用，调到条高一半就是完全胶囊形\ncorner_radius=14\n",
        ),
        (
            "theme",
            "# 外观主题：calm_light/calm_dark（平静极简）/ glass2_light/glass2_dark（玻璃 2.0）\n\
             # m3_light/m3_dark（Material 3）；设置页里点色块切换\ntheme=calm_light\n",
        ),
        ("pill_anchor", "# 胶囊位置形状：float / island（吸顶刘海）\npill_anchor=float\n"),
        ("dock_anchor", "# Dock 靠屏幕哪条边：bottom / top / left / right\ndock_anchor=bottom\n"),
        ("weather_source", "# 天气数据源：amap / open_meteo\nweather_source=amap\namap_key=\n"),
        ("imap_host", "# 邮箱 IMAP 账号（留空回落 data/mail.txt）\nimap_host=\nimap_port=\nimap_user=\nimap_pass=\n"),
        ("lyric_position", "# 音乐条位置：right（胶囊右侧，默认）/ left（左侧）/ bottom（下方）\nlyric_position=right\n"),
        ("lyric_size", "# 歌词字号\nlyric_size=20\n"),
        ("weather_text_scale", "# 天气面板字号倍率：1.0 为基准\nweather_text_scale=1.25\n"),
        ("weather_scheme", "# 天气卡片配色：0 自动跟壁纸（默认）/ 1 深色背景（浅色字）/ 2 浅色背景（深色字）\nweather_scheme=0\n"),
        (
            "show_tray",
            "# 底部 Dock：显示托盘图标 / 右端“返回桌面”细长条（1 = 显示，0 = 隐藏）\nshow_tray=1\nshow_desktop_btn=1\n",
        ),
        ("hide_tray", "# Dock 上要单独隐藏的托盘图标（分号分隔，键由设置页生成；清空 = 全部显示）\nhide_tray=\n"),
        ("tray_order", "# 托盘图标自定义排序（稳定键列表，分号分隔，设置页 ▲▼ 排序写回；清空 = 默认顺序）\ntray_order=\n"),
        ("wechat_sound", "# 微信托盘图标开始闪烁（新消息）时播放内置提示音（1 = 开，默认；0 = 关）\nwechat_sound=1\n"),
        ("wechat_cards", "# 微信消息卡片（UIA 读会话名+摘要，1 = 开；0 = 关，默认）\nwechat_cards=0\n"),
        ("wechat_banner_jump", "# 点击微信消息横幅是否直达对应会话（1 = 开；0 = 关，默认）。开启后点横幅会切到该对话并把它标为已读；关闭则只唤出微信窗口\nwechat_banner_jump=0\n"),
        ("wechat_hover_preview", "# 微信托盘悬停预览（用 Orbit 自绘浮窗替代微信自己的悬停小窗；1 = 开；0 = 关，默认）\nwechat_hover_preview=0\n"),
        ("wechat_ignore", "# 微信卡片白名单：标题命中就不弹卡片（英文逗号分隔；默认「折叠的聊天」）\nwechat_ignore=折叠的聊天\n"),
        ("wechat_hotkeys", "# 微信「显示↔最小化」热键（英文逗号分隔，按微信启动先后对应：第 1 个 = 先开的微信）\n# 例：F1,F2 或 Ctrl+Alt+1,Ctrl+Alt+2；设置页「通知接管（微信）」里可直接改。只做最小化，绝不隐藏\nwechat_hotkeys=\n"),
        ("wechat_hotkey_bind", "# 微信热键「按账号」绑定：`账号ID=热键`，英文逗号分隔。账号ID 见设置页「账号监控」列表（data/wechat_accounts 下的目录名）\n# 绑到账号跟账号走，换启动顺序也不会串；没做快照的实例用上面的 wechat_hotkeys 按启动顺序兜底\n# 例：1234567890=F1,9876543210=Ctrl+Alt+2。设置页账号列表每行直接填，不用手改这里\nwechat_hotkey_bind=\n"),
        ("wechat_path", "# 微信主程序完整路径（到 Weixin.exe 为止）；空 = 自动探测（运行中进程 → 常见位置）\n# 例：D:\\Tencent\\Weixin\\weixin\\Weixin.exe\nwechat_path=\n"),
        ("wechat_data_dir", "# 微信数据目录（提供 xwechat_files 目录即可）；空 = 自动探测\n# 例：D:\\Tencent\\Weixin\\xwechat_files\nwechat_data_dir=\n"),
        ("wechat_close_min", "# 点微信「关闭」时改成最小化而不是收进托盘（1 = 开，默认；0 = 关）\nwechat_close_min=1\n"),
        ("banner_secs", "# 右下角横幅每条停留秒数（0 = 不自动关闭，默认 6）\nbanner_secs=6\n"),
        ("lyric_lead", "# 歌词提前量（秒，默认 1；0 = 不提前；负值 = 延后；设置页按 0.1 秒调）\nlyric_lead=1\n"),
        ("font", "# 界面字体：misans = MiSans（随包分发，默认）；system = 系统字体\nfont=misans\n"),
        ("start_pins", "# 开始菜单（Win 键弹出）快捷方式：`名称|程序路径`，@admin: 前缀提权；删掉本行恢复默认\nstart_pins=命令提示符(管理员)|@admin:C:\\Windows\\System32\\cmd.exe\n"),
        ("autostart", "# 开机启动（1 = 开，默认；写注册表 HKCU Run 键）\nautostart=1\n"),
        ("guardian", "# 退出守护：orbit 被强杀后由守护进程重启 explorer 恢复任务栏（1 = 开，默认）\nguardian=1\n"),
        (
            "tray_fake_input",
            "# 伪造托盘点击/悬停（回调消息投递，1 = 开，默认）；关掉做透明转发 A/B\ntray_fake_input=1\n",
        ),
        (
            "notify_wechat",
            "# 通知通道（每个来源独立）：off / orbit（横幅）/ windows（系统通知）\n\
             notify_wechat=orbit\nnotify_tray=orbit\nnotify_mail=orbit\nnotify_clipboard=orbit\n",
        ),
        (
            "banner_wechat_secs",
            "# 各来源横幅停留秒数（0 = 不消失）\n\
             banner_wechat_secs=6\nbanner_mail_secs=6\nbanner_tray_secs=6\nbanner_clipboard_secs=6\n",
        ),
        (
            "sound_mode",
            "# 提示音模式：off / once / twice / thrice / until（直到消息消失）\nsound_mode=once\n\
             # 接管 Windows 通知中心（1 = 开）\nnotify_takeover=0\n",
        ),
        (
            "mail_accounts",
            "# 邮件多账号（host|port|user|pass，; 分隔；非空优先于 imap_*）\n\
             mail_accounts=\nmail_verified=\n",
        ),
        ("clipboard_pinned", "# 剪切板面板常驻置顶文字\nclipboard_pinned=\n"),
        (
            "start_hook",
            "# 开始菜单：Win 键接管（1 = 接管，默认）/ 每行个数（1 ~ 8）\n\
             start_hook=1\nstart_cols=4\n",
        ),
    ] {
        if !has(key) && !(key == "alpha" && legacy_alpha) {
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
    let mut wscheme = 0u32;
    let mut size = cfg::LYRIC_FONT_SIZE;
    let mut llead: Option<f32> = None;
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
    let mut wcards = false;
    let mut wpreview = false;
    let mut wjump = false;
    let mut wcmin = true;
    let mut misans = true;
    // None = 文件里没这个键 → 保留默认（折叠的聊天）；Some(空 Vec) = 用户清空了白名单
    let mut wignore: Option<Vec<String>> = None;
    // None = 文件里没这个键 → 保留默认（空 = 不注册热键）
    let mut whotkeys: Option<Vec<String>> = None;
    let mut wbind: Option<Vec<(String, String)>> = None;
    let mut wpath: Option<String> = None;
    let mut wdatadir: Option<String> = None;
    let mut bsecs: Option<u32> = None;
    let mut hide_tray: Vec<String> = Vec::new();
    let mut torder: Vec<String> = Vec::new();
    let mut dpins: Option<Vec<String>> = None;
    let mut iover: Vec<(String, String)> = Vec::new();
    let mut spins: Option<Vec<(String, String)>> = None;
    let mut auto = true;
    let mut guard_on = true;
    let mut fake_input = cfg::TRAY_FAKE_INPUT;
    let mut sys_on = false;
    // 新键 `alpha` 优先；老键 pill_alpha / dock_alpha 只作迁移兜底
    let mut alpha_new: Option<f32> = None;
    let mut alpha_legacy = 1.0f32;
    let mut pscale = 1.0f32;
    let mut iscale = 1.0f32;
    let mut bscale = 1.0f32;
    let mut corner = CORNER_DEFAULT;
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
    // 新键：通知 / 横幅 / 提示音 / 多账号 / Dock / 开始菜单 / 剪切板
    let mut nw = NotifyChannel::Orbit.code();
    let mut nt = NotifyChannel::Orbit.code();
    let mut nm = NotifyChannel::Orbit.code();
    let mut nc = NotifyChannel::Orbit.code();
    let mut bw = 6u32;
    let mut bm = 6u32;
    let mut bt = 6u32;
    let mut bc = 6u32;
    let mut sm = SoundMode::Once.code();
    let mut mail_accs: Vec<MailAccount> = Vec::new();
    let mut mail_ver: Vec<String> = Vec::new();
    let mut clip_pin = String::new();
    let mut shook = true;
    let mut takeover = false;
    let mut scols = 4usize;
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
            "alpha" => {
                if let Some(n) = parse_num(v) {
                    alpha_new = Some(n.clamp(ALPHA_MIN, ALPHA_MAX));
                }
            }
            // 老键：合并成一个界面透明度（胶囊/模块条取 pill_alpha，Dock 取 dock_alpha，
            // 两个都有时取最小值，免得升级后某块界面突然变实）
            "pill_alpha" | "dock_alpha" => {
                if let Some(n) = parse_num(v) {
                    let n = n.clamp(ALPHA_MIN, ALPHA_MAX);
                    alpha_legacy = alpha_legacy.min(n);
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
            "banner_scale" => {
                if let Some(n) = parse_num(v) {
                    bscale = n.clamp(BANNER_SCALE_MIN, BANNER_SCALE_MAX);
                }
            }
            "corner_radius" => {
                if let Some(n) = parse_num(v) {
                    corner = n.clamp(CORNER_MIN, CORNER_MAX);
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
            "tray_order" => torder = parse_hide(v),
            "dock_pins" => dpins = Some(parse_list(v)),
            "icon_over" => iover = parse_pairs(v),
            "start_pins" => spins = Some(parse_pairs(v)),
            "autostart" => {
                if let Some(b) = parse_bool(v) {
                    auto = b;
                }
            }
            "guardian" => {
                if let Some(b) = parse_bool(v) {
                    guard_on = b;
                }
            }
            "tray_fake_input" => {
                if let Some(b) = parse_bool(v) {
                    fake_input = b;
                }
            }
            "sys_panel" => {
                if let Some(b) = parse_bool(v) {
                    sys_on = b;
                }
            }
            "weather_text_scale" => {
                if let Some(n) = parse_num(v) {
                    scale = n.clamp(SCALE_MIN, SCALE_MAX);
                }
            }
            "weather_scheme" => {
                if let Some(n) = parse_num(v) {
                    wscheme = (n.max(0.0) as u32).min(2);
                }
            }
            "lyric_size" => {
                if let Some(n) = parse_num(v) {
                    size = n.clamp(LYRIC_MIN, LYRIC_MAX);
                }
            }
            "lyric_lead" => {
                if let Some(n) = parse_num(v) {
                    llead = Some(n.clamp(-LYRIC_LEAD_MAX, LYRIC_LEAD_MAX));
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
            "wechat_cards" => {
                if let Some(b) = parse_bool(v) {
                    wcards = b;
                }
            }
            "wechat_hover_preview" => {
                if let Some(b) = parse_bool(v) {
                    wpreview = b;
                }
            }
            "wechat_banner_jump" => {
                if let Some(b) = parse_bool(v) {
                    wjump = b;
                }
            }
            "wechat_ignore" => {
                // 英文逗号分隔
                wignore = Some(
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
            }
            "wechat_hotkeys" => {
                whotkeys = Some(
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
            }
            "wechat_hotkey_bind" => {
                // `账号id=热键` 逗号分隔；没有 `=` 的项直接忽略
                wbind = Some(
                    v.split(',')
                        .filter_map(|it| it.split_once('='))
                        .map(|(k, s)| (k.trim().to_string(), s.trim().to_string()))
                        .filter(|(k, s)| !k.is_empty() && !s.is_empty())
                        .collect(),
                );
            }
            "wechat_path" => {
                wpath = Some(v.trim().to_string());
            }
            "wechat_data_dir" => {
                wdatadir = Some(v.trim().to_string());
            }
            "wechat_close_min" => {
                if let Some(b) = parse_bool(v) {
                    wcmin = b;
                }
            }
            "banner_secs" => {
                bsecs = v.trim().parse::<u32>().ok();
            }
            "font" => {
                misans = !v.trim().eq_ignore_ascii_case("system");
            }
            "notify_wechat" => {
                if let Some(c) = NotifyChannel::parse(v) { nw = c.code(); }
            }
            "notify_tray" => {
                if let Some(c) = NotifyChannel::parse(v) { nt = c.code(); }
            }
            "notify_mail" => {
                if let Some(c) = NotifyChannel::parse(v) { nm = c.code(); }
            }
            "notify_clipboard" => {
                if let Some(c) = NotifyChannel::parse(v) { nc = c.code(); }
            }
            "banner_wechat_secs" => {
                bw = v.trim().parse::<u32>().unwrap_or(6).min(3600);
            }
            "banner_mail_secs" => {
                bm = v.trim().parse::<u32>().unwrap_or(6).min(3600);
            }
            "banner_tray_secs" => {
                bt = v.trim().parse::<u32>().unwrap_or(6).min(3600);
            }
            "banner_clipboard_secs" => {
                bc = v.trim().parse::<u32>().unwrap_or(6).min(3600);
            }
            "sound_mode" => {
                if let Some(m) = SoundMode::parse(v) { sm = m.code(); }
            }
            "notify_takeover" => {
                if let Some(b) = parse_bool(v) { takeover = b; }
            }
            "mail_accounts" => {
                mail_accs = v.split(';').filter_map(MailAccount::parse).collect();
            }
            "mail_verified" => {
                mail_ver = v
                    .split(';')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "clipboard_pinned" => clip_pin = v.to_string(),
            "start_hook" => {
                if let Some(b) = parse_bool(v) { shook = b; }
            }
            "start_cols" => {
                if let Ok(n) = v.trim().parse::<usize>() { scols = n.clamp(1, 8); }
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
    WEATHER_SCHEME.store(wscheme, Ordering::Relaxed);
    LYRIC_SIZE.store(size.to_bits(), Ordering::Relaxed);
    if let Some(v) = llead {
        LYRIC_LEAD.store(v.to_bits(), Ordering::Relaxed);
    }
    LYRIC_POS.store(pos.code(), Ordering::Relaxed);
    if let Ok(mut v) = CLOCK_FORMAT.lock() {
        *v = fmt;
    }
    if let Ok(mut v) = MODULE_ORDER.lock() {
        *v = order;
    }
    let alpha = alpha_new.unwrap_or(alpha_legacy);
    UI_ALPHA.store(alpha.to_bits(), Ordering::Relaxed);
    PILL_SCALE.store(pscale.to_bits(), Ordering::Relaxed);
    ICON_SCALE.store(iscale.to_bits(), Ordering::Relaxed);
    BANNER_SCALE.store(bscale.to_bits(), Ordering::Relaxed);
    CORNER.store(corner.to_bits(), Ordering::Relaxed);
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
    WECHAT_CARDS.store(bool_code(wcards), Ordering::Relaxed);
    WECHAT_HOVER.store(bool_code(wpreview), Ordering::Relaxed);
    WECHAT_JUMP.store(bool_code(wjump), Ordering::Relaxed);
    FONT.store(if misans { 1 } else { 2 }, Ordering::Relaxed);
    if let Some(list) = wignore {
        if let Ok(mut v) = WECHAT_IGNORE.lock() {
            *v = list;
        }
    }
    if let Some(list) = whotkeys {
        if let Ok(mut v) = WECHAT_HOTKEYS.lock() {
            *v = list;
        }
    }
    if let Some(list) = wbind {
        if let Ok(mut v) = WECHAT_HOTKEY_BIND.lock() {
            *v = list;
        }
    }
    if let Some(s) = wpath {
        if let Ok(mut v) = WECHAT_PATH.lock() {
            *v = s;
        }
    }
    if let Some(s) = wdatadir {
        if let Ok(mut v) = WECHAT_DATA_DIR.lock() {
            *v = s;
        }
    }
    WECHAT_CLOSE_MIN.store(bool_code(wcmin), Ordering::Relaxed);
    if let Some(s) = bsecs {
        BANNER_SECS.store(s.min(3600) + 1, Ordering::Relaxed);
    }
    if let Ok(mut v) = HIDE_TRAY.lock() {
        *v = hide_tray;
    }
    if let Ok(mut v) = TRAY_ORDER.lock() {
        *v = torder;
    }
    if let Some(p) = dpins {
        if let Ok(mut v) = DOCK_PINS.lock() {
            *v = Some(p);
        }
    }
    if let Ok(mut v) = ICON_OVER.lock() {
        *v = iover;
    }
    if let Ok(mut v) = START_PINS.lock() {
        *v = spins;
    }
    AUTOSTART.store(bool_code(auto), Ordering::Relaxed);
    GUARDIAN.store(bool_code(guard_on), Ordering::Relaxed);
    TRAY_FAKE_INPUT.store(bool_code(fake_input), Ordering::Relaxed);
    SYS_PANEL.store(bool_code(sys_on), Ordering::Relaxed);
    // 新键
    NOTIFY_WECHAT.store(nw, Ordering::Relaxed);
    NOTIFY_TRAY.store(nt, Ordering::Relaxed);
    NOTIFY_MAIL.store(nm, Ordering::Relaxed);
    NOTIFY_CLIPBOARD.store(nc, Ordering::Relaxed);
    BANNER_WECHAT_SECS.store(bw + 1, Ordering::Relaxed);
    BANNER_MAIL_SECS.store(bm + 1, Ordering::Relaxed);
    BANNER_TRAY_SECS.store(bt + 1, Ordering::Relaxed);
    BANNER_CLIPBOARD_SECS.store(bc + 1, Ordering::Relaxed);
    SOUND_MODE.store(sm, Ordering::Relaxed);
    NOTIFY_TAKEOVER.store(bool_code(takeover), Ordering::Relaxed);
    if let Ok(mut v) = MAIL_ACCOUNTS.lock() { *v = mail_accs; }
    if let Ok(mut v) = MAIL_VERIFIED.lock() { *v = mail_ver; }
    if let Ok(mut v) = CLIPBOARD_PINNED_TEXT.lock() { *v = clip_pin; }
    START_HOOK.store(bool_code(shook), Ordering::Relaxed);
    START_COLS.store(scols as u32, Ordering::Relaxed);
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

/// `键|值` 对，分号分隔（`icon_over` 用）。键和值里都不允许出现 `|` 或 `;`
fn parse_pairs(v: &str) -> Vec<(String, String)> {
    v.split(';')
        .filter_map(|item| {
            let (k, p) = item.split_once('|')?;
            let (k, p) = (k.trim(), p.trim());
            (!k.is_empty() && !p.is_empty()).then(|| (k.to_string(), p.to_string()))
        })
        .collect()
}

/// 分号分隔的字符串列表（`hide_tray` 的图标键、`dock_pins` 的路径都走它）
fn parse_list(v: &str) -> Vec<String> {
    v.split(';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// 隐藏列表写盘用 `;` 分隔；读到空值 = 全部显示
fn parse_hide(v: &str) -> Vec<String> {
    parse_list(v)
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
        // 老配置里的「吸底」已取消：读到就当悬浮，别让整条设置失效
        assert_eq!(PillAnchor::parse("吸底"), Some(PillAnchor::Float));
        assert_eq!(PillAnchor::parse("bottom"), Some(PillAnchor::Float));
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
        assert_eq!(PillAnchor::from_code(3), PillAnchor::Float); // 老值 3 = 吸底
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
    fn semicolon_lists_are_trimmed() {
        assert_eq!(
            parse_list(" C:/a.exe ; D:/b.lnk ;;"),
            vec!["C:/a.exe".to_string(), "D:/b.lnk".to_string()]
        );
        assert!(parse_list("").is_empty());
        assert!(parse_list(" ; ; ").is_empty());
    }

    #[test]
    fn icon_over_pairs_keep_windows_paths() {
        // 键里带盘符冒号和 `#uid`，值里带盘符冒号：只能用 `|` 分隔，别拿 `:` 或 `=` 切
        let v = "e:d:\\tencent\\workbuddy\\workbuddy.exe#3|D:/sh\\ic.png;\
                 g:GUID(0x1, 0x2, 0x3, [0x4])|C:\\a\\b.ico";
        let got = parse_pairs(v);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "e:d:\\tencent\\workbuddy\\workbuddy.exe#3");
        assert_eq!(got[0].1, "D:/sh\\ic.png");
        assert_eq!(got[1].1, "C:\\a\\b.ico");
        // 缺 `|` 或空白的半边直接丢掉，不要产生 "键→空路径" 这种脏数据
        assert!(parse_pairs("only_key").is_empty());
        assert!(parse_pairs("k|").is_empty());
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
    fn wechat_ignore_default_blocks_folded_chat() {
        assert!(is_wechat_ignored("折叠的聊天"));
        assert!(is_wechat_ignored(" 折叠的聊天 "));
        assert!(!is_wechat_ignored("张三"));
        assert!(!is_wechat_ignored("折叠的聊天通知"));
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
