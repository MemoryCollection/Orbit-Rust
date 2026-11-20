//! 全局样式与行为常量：所有尺寸/颜色/透明度集中在这里调。
//! 数值为 96 DPI 基准，物理像素 = 值 × scale。

/// 胶囊高度
pub const PILL_H: f32 = 44.0;
/// 左右内边距
pub const PAD_X: f32 = 18.0;
/// 时间字号（粗体）
pub const TIME_SIZE: f32 = 20.0;
/// 日期字号（粗体）
pub const DATE_SIZE: f32 = 12.0;
/// 元素间距
pub const GAP: f32 = 10.0;
/// 图标悬停背景边长
pub const ICON_BG: f32 = 26.0;
/// 分隔线高度
pub const DIVIDER_H: f32 = 22.0;
/// 胶囊距屏幕顶部距离
pub const TOP_MARGIN: i32 = 6;

/// 胶囊背景（近黑，205/255 不透明）
pub const BG_ARGB: u32 = 0xCD_1C_1C_1A;
/// 时间文字（纯白）
pub const TIME_ARGB: u32 = 0xFF_FF_FF_FF;
/// 日期文字（75% 白）
pub const DATE_ARGB: u32 = 0xBF_FF_FF_FF;
/// 分隔线（22% 白）
pub const DIVIDER_ARGB: u32 = 0x38_FF_FF_FF;
/// 悬停背景（20% 白）
pub const HOVER_ARGB: u32 = 0x33_FF_FF_FF;

/// 托盘图标区总开关（托盘区在底部 Dock 右侧，不在顶部胶囊里）
pub const SHOW_TRAY: bool = true;

// ---------- 胶囊 widget ----------

/// 天气 widget 开关
pub const SHOW_WEATHER: bool = true;
/// 音乐条（封面 + 歌词，胶囊旁那条）开关；原胶囊里的音乐 widget 已合并到音乐条
pub const SHOW_MUSIC: bool = true;
/// 剪贴板 widget 开关
pub const SHOW_CLIPBOARD: bool = true;
/// widget 符号图标字号（Segoe UI Symbol）
pub const WIDGET_ICON_SIZE: f32 = 15.0;
/// widget 文本字号
pub const WIDGET_TEXT_SIZE: f32 = 13.0;
/// 符号与文本之间的间距
pub const WIDGET_ICON_GAP: f32 = 4.0;
/// widget 悬停背景的左右内边距
pub const WIDGET_PAD_X: f32 = 5.0;
/// widget 文本（90% 白）
pub const TEXT_ARGB: u32 = 0xE6_FF_FF_FF;
/// widget 图标（70% 白）
pub const TEXT_DIM_ARGB: u32 = 0xB3_FF_FF_FF;
/// 天气图标：白天晴天（暖金）
pub const SUN_ARGB: u32 = 0xFF_FF_C4_54;
/// 天气图标：夜间（淡蓝白）
pub const MOON_ARGB: u32 = 0xFF_BC_D4_FF;
/// 天气图标：雨雪雷（冷蓝）
pub const RAIN_ARGB: u32 = 0xFF_86_B8_F0;
/// 天气图标：阴/多云（浅灰）
pub const CLOUD_ARGB: u32 = 0xFF_C9_CF_D6;
/// 天气目标城市（留空 = 按出口 IP 自动定位）
pub const WEATHER_CITY: &str = "";
/// 高德 Web 服务 key（留空则只用 Open-Meteo）。免费额度 5000 次/月，
/// 每次刷新消耗 2 次（实况 + 预报），30 分钟一次约 2900 次/月。
pub const AMAP_KEY: &str = "";
/// 天气刷新间隔（秒）
pub const WEATHER_INTERVAL: u64 = 1800;

// ---------- 邮箱 ----------

/// 邮箱 widget 开关
pub const SHOW_MAIL: bool = true;
/// IMAP 服务器（QQ 邮箱）
pub const MAIL_HOST: &str = "imap.qq.com";
pub const MAIL_PORT: u16 = 993;
/// 账号与授权码：首次运行会写入 <exe>/data/mail.txt，之后以该文件为准
pub const MAIL_USER: &str = "";
pub const MAIL_PASS: &str = "";
/// 拉取间隔（秒）
pub const MAIL_INTERVAL: u64 = 90;
/// 每次拉取的邮件条数
pub const MAIL_FETCH: usize = 6;

// ---------- 展开面板 ----------

/// 面板背景（不透明：面板压在窗口上，半透明会被背景文字透出来）
pub const PANEL_BG_ARGB: u32 = 0xFF_1E_1E_1C;
/// 面板描边
pub const PANEL_BORDER_ARGB: u32 = 0x33_FF_FF_FF;
/// 圆角半径
pub const PANEL_RADIUS: f32 = 14.0;
/// 内边距
pub const PANEL_PAD: f32 = 16.0;
/// 标题区高度（含副标题）
pub const PANEL_HEAD_H: f32 = 58.0;
/// 每行高度
pub const PANEL_ROW_H: f32 = 46.0;
/// 面板最小/最大宽度
pub const PANEL_MIN_W: f32 = 210.0;
pub const PANEL_MAX_W: f32 = 360.0;
/// 面板与胶囊的间距
pub const PANEL_GAP: i32 = 6;
/// 标题字号 / 副标题字号
pub const PANEL_TITLE_SIZE: f32 = 14.5;
pub const PANEL_SUB_SIZE: f32 = 11.5;
/// 行内标题 / 说明 / 右侧数值字号
pub const PANEL_ROW_TITLE: f32 = 13.5;
pub const PANEL_ROW_SUB: f32 = 11.5;
pub const PANEL_VALUE_SIZE: f32 = 13.0;
/// 图标与文字的间距
pub const PANEL_ICON_GAP: f32 = 8.0;
/// 标题与右侧数值的间距
pub const PANEL_VALUE_GAP: f32 = 14.0;
/// 强调色（"天气"二字、降水概率）
pub const ACCENT_ARGB: u32 = 0xFF_5A_A9_FF;

// ---------- 天气面板（宽卡片，左当前 / 右四天）----------

pub const WEATHER_PANEL_W: f32 = 560.0;
/// 卡片高度基准（实际高度取 `WEATHER_PANEL_H * 0.88 * 字号倍率`，左栏更高时再撑高）
pub const WEATHER_PANEL_H: f32 = 190.0;
/// 左栏（实况）宽度：图标 + 文字列
pub const WEATHER_LEFT_W: f32 = 200.0;
/// 大字号温度
pub const WEATHER_TEMP_SIZE: f32 = 34.0;
/// 左侧大图标
pub const WEATHER_ICON_SIZE: f32 = 54.0;
/// 天气描述（温度下面那行，与城市同一行）
pub const WEATHER_COND_SIZE: f32 = 14.5;
/// 城市字号（比天气描述小一号，压暗）
pub const WEATHER_CITY_SIZE: f32 = 12.0;
pub const WEATHER_DETAIL_SIZE: f32 = 11.5;
/// 右侧每天列的字号
pub const WEATHER_DAY_LABEL: f32 = 12.5;
pub const WEATHER_DAY_ICON: f32 = 19.0;
pub const WEATHER_DAY_HIGH: f32 = 16.0;
pub const WEATHER_DAY_LOW: f32 = 12.5;
/// 每日天气文字与风力（比上面更小一档）
pub const WEATHER_DAY_TEXT: f32 = 11.5;
pub const WEATHER_DAY_WIND: f32 = 11.0;
/// 天气面板字号倍率的默认值：真正的值从 <exe>/data/settings.txt 的
/// `weather_text_scale` 读（见 settings.rs），这里只是缺省。
pub const WEATHER_TEXT_SCALE: f32 = 1.25;

// ---------- 底部 Dock ----------

/// Dock 总开关
pub const SHOW_DOCK: bool = true;
/// Dock 高度
pub const DOCK_H: f32 = 56.0;
/// Dock 图标边长
pub const DOCK_ICON: f32 = 44.0;
/// 图标槽位（悬停底 = 图标 + 2px）
pub const DOCK_SLOT: f32 = 46.0;
/// 相邻图标间距
pub const DOCK_GAP: f32 = 6.0;
/// 左右内边距
pub const DOCK_PAD_X: f32 = 8.0;
/// 圆角半径
pub const DOCK_RADIUS: f32 = 18.0;
/// 距屏幕底部距离
pub const DOCK_BOTTOM_MARGIN: i32 = 8;
/// 运行中窗口重扫间隔（毫秒）
pub const DOCK_POLL_MS: u32 = 2000;
/// Dock 托盘区：图标边长（与任务图标一致，视觉上才是一排）
/// 源像素只有 16~24px，放大走 Catmull-Rom（见 tray::resize_premul）
pub const DOCK_TRAY_ICON: f32 = DOCK_ICON;
/// Dock 托盘区：槽位（与任务图标槽位一致）
pub const DOCK_TRAY_SLOT: f32 = DOCK_SLOT;
/// Dock 托盘区：图标间距
pub const DOCK_TRAY_GAP: f32 = DOCK_GAP;
/// Dock 托盘区与左侧应用图标之间的间距（分隔线两侧各留这么多）
pub const DOCK_TRAY_SEP: f32 = 8.0;

// ---------- 托盘镜像 ----------

/// 托盘托管（对齐 C# Orbit）：自建假 Shell_TrayWnd + 广播 TaskbarCreated，让应用把
/// 图标重新注册到我们这里。图标 hIcon/tooltip/回调消息/版本/GUID 全部从 NOTIFYICONDATA
/// 直接拿到，不依赖 UIA——这是"隐藏任务栏 + 悬停弹卡片 + 镜像"能同时成立的前提。
/// 关闭则退回旧的 UIA 镜像（只适合保留真任务栏调试）。
pub const TRAY_TAKEOVER: bool = true;
/// 图标优先取程序 exe 里的 256px 大图标（比从任务栏抠 16px 再放大清晰得多）
pub const TRAY_USE_EXE_ICON: bool = true;
/// exe 图标与任务栏抓图的允许差异（0~1）。超过就认为不是同一个图标
/// （输入法这类"状态图标"的 exe 图标完全不是一回事），保留抓图
pub const TRAY_ICON_MAX_DIFF: f32 = 0.34;
/// 闪烁采样周期（毫秒）：应用用"图标一闪一闪"提示新消息时，Dock 按同样节奏跟随
pub const TRAY_FLASH_MS: u32 = 70;
/// 把"鼠标停在 Dock 的托盘图标上"转发成真实图标的悬停：向图标的宿主窗口投它注册时的
/// 回调消息（wParam/lParam 按 uVersion 0/3/4 编码，坐标用图标矩形中心），**不移动光标**。
/// 回调消息号系统没有 API 可查，靠 `traymsg` 从目标进程内存里挖 `NOTIFYICONDATAW`，
/// 结果按 exe 缓存进 `orbit.traycache`（每个程序只需扫一次）。
pub const TRAY_FORWARD_HOVER: bool = true;

/// 悬停转发后，应用自己弹的预览小窗（微信那种卡片）会落在**旧托盘角**。
/// 打开这个就把那个 tool window 摆到 Dock 正上方，并在它开着的时候一直守着别再漂回去。
/// 移植自 C# 版 Orbit 的 `HoverFlyoutRelocator`，实现见 `src/flyout.rs`。
pub const TRAY_RELOCATE_FLYOUT: bool = true;

// ---------- 悬停提示条（托盘/任务图标 tooltip）----------

/// 提示条字号
pub const TIP_TEXT_SIZE: f32 = 12.5;
/// 提示条左右内边距
pub const TIP_PAD_X: f32 = 10.0;
/// 提示条上下内边距
pub const TIP_PAD_Y: f32 = 5.0;
/// 提示条圆角
pub const TIP_RADIUS: f32 = 8.0;
/// 提示条与 Dock 的间距
pub const TIP_GAP: i32 = 6;
/// 提示条最大宽度（超出按省略号截断）
pub const TIP_MAX_W: f32 = 260.0;
/// 提示条背景（比 Dock 更实一点，压在桌面/窗口上也读得清）
pub const TIP_BG_ARGB: u32 = 0xF2_1C_1C_1A;
/// 提示条文字
pub const TIP_TEXT_ARGB: u32 = 0xFF_FF_FF_FF;

// ---------- 音乐条（旋转封面 + 歌词）----------

/// 歌词字号（逻辑单位；可由 settings.txt 的 lyric_size 覆盖）
pub const LYRIC_FONT_SIZE: f32 = 14.0;
/// 音乐条左右内边距
pub const LYRIC_PAD_X: f32 = 16.0;
/// 音乐条与胶囊之间的间距（像素）
pub const LYRIC_GAP: i32 = 8;
/// 音乐条最大宽度（超出按省略号截断）
pub const LYRIC_MAX_W: f32 = 560.0;
/// 句首音符符号的字号（无歌词/无封面时的占位）
pub const LYRIC_ICON_SIZE: f32 = 13.0;
/// 封面直径
pub const MUSIC_COVER: f32 = 32.0;
/// 封面距左边缘的距离（圆形比文字更贴边，视觉上才不显空）
pub const MUSIC_COVER_PAD: f32 = 9.0;
/// 封面与歌词之间的间距
pub const MUSIC_COVER_GAP: f32 = 12.0;
/// 封面中心的播放/暂停按钮直径（占封面直径的比例）
pub const MUSIC_BTN_RATIO: f32 = 0.62;
/// 封面旋转速度（度/秒，只在播放时转，约 8 秒一圈）
pub const COVER_DEG_PER_SEC: f32 = 45.0;
/// 没有专辑封面时的占位音符
pub const MUSIC_PLACEHOLDER: &str = "♪";

// ---------- 剪贴板面板 ----------

/// 固定（置顶）标记的宽度：点这里切换固定状态
pub const PIN_W: f32 = 26.0;
/// 固定的条目在列表中用强调色星标
pub const PINNED_ARGB: u32 = 0xFF_FF_D5_4F;
