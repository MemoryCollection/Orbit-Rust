//! 全局样式与行为常量：所有尺寸/颜色/透明度集中在这里调。
//! 数值为 96 DPI 基准，物理像素 = 值 × scale。
//!
//! **尺寸**（字号 / 间距 / 高度）是编译期常量；**颜色与圆角**已抽到 `crate::theme`，
//! 取法是 `cfg::cur().pill_bg` 这样——换一套主题不用改这里，设置页切换即可。

pub use crate::theme::cur;

/// 按「界面透明度」调过的**板面底色**（只改 alpha 字节，RGB 原样）。
///
/// 这里的语义是「背景有多不透明」：滑条 100% → 底色完全不透明（主题自带的
/// 半透明 `pill_bg` / `panel_bg` 也不再透出桌面），滑条 50% → 板面半透明。
///
/// **文字、图标、描边一律不参与**：它们照原色画在板面之上，所以拉低滑条只会让
/// 「背景」透出桌面，字和图标始终清晰可读。
pub fn bg_alpha(c: u32) -> u32 {
    let a = crate::settings::ui_alpha().clamp(0.0, 1.0);
    if a >= 0.999 {
        return opaque(c);
    }
    let alpha = ((a * 255.0).round() as u32).min(0xFF);
    (alpha << 24) | (c & 0x00FF_FFFF)
}

/// 胶囊 / 模块条 / 音乐条 / Dock / 开始菜单 / 横幅的板面底色（主题 `pill_bg`）。
pub fn ui_bg_effective() -> u32 {
    bg_alpha(cur().pill_bg)
}

/// 只把 alpha 字节提到 0xFF，RGB 原样保留
pub fn opaque(c: u32) -> u32 {
    (0xFF << 24) | (c & 0x00FF_FFFF)
}

/// 胶囊高度
pub const PILL_H: f32 = 44.0;
/// 左右内边距
pub const PAD_X: f32 = 18.0;
/// 时钟字号（粗体）：日期与时间**合并成一段**，整段共用这个字号
pub const TIME_SIZE: f32 = 20.0;
/// 元素间距（时钟 ↔ 模块、模块 ↔ 模块）
pub const GAP: f32 = 7.0;
/// 图标悬停背景边长
pub const ICON_BG: f32 = 26.0;
/// 分隔线高度
pub const DIVIDER_H: f32 = 22.0;
/// 胶囊距屏幕顶部距离
pub const TOP_MARGIN: i32 = 6;

// 颜色统一走 `cfg::cur().*`（见 theme.rs）：pill_bg / text / text_dim / divider / hover …

/// 托盘图标区总开关（托盘区在底部 Dock 右侧，不在顶部胶囊里）
pub const SHOW_TRAY: bool = true;

// ---------- 胶囊 widget ----------

/// 音乐条（封面 + 歌词，胶囊旁那条）开关；原胶囊里的音乐 widget 已合并到音乐条
pub const SHOW_MUSIC: bool = true;
/// 剪贴板 widget 开关
pub const SHOW_CLIPBOARD: bool = true;
/// USB widget 开关：插着可移动磁盘（U 盘 / 读卡器）时才出现在胶囊上
pub const SHOW_USB: bool = true;
/// 分离出来的模块条与胶囊之间的间距（多条之间也用这个间距）
pub const WIDGET_BAR_GAP: i32 = 6;
/// 每侧最多分几条（= `slot` 取值上限，也决定 widgetbar 预建的窗口数）
pub const WIDGET_BAR_MAX_SLOT: u8 = 3;
/// 模块条的左右内边距
pub const WIDGET_BAR_PAD_X: f32 = 8.0;
/// 模块条内相邻 widget 的间距
pub const WIDGET_BAR_ITEM_GAP: f32 = 3.0;
/// widget 符号图标字号（Segoe UI Symbol，只作为字形蒙版的渲染基准）
pub const WIDGET_ICON_SIZE: f32 = 15.0;
/// 图标格子边长：所有 widget 图标都占同样大的格子，间距才均匀
pub const WIDGET_ICON_CELL: f32 = 16.0;
/// widget 文本字号
pub const WIDGET_TEXT_SIZE: f32 = 13.0;
/// 符号与文本之间的间距
pub const WIDGET_ICON_GAP: f32 = 3.0;
/// widget 悬停背景的左右内边距
pub const WIDGET_PAD_X: f32 = 4.0;
/// 相邻两个 widget 之间的间距（不含两侧 pad）。
/// 注意：egui 的 `item_spacing` 会再叠一层，胶囊布局里已显式置 0，间距只由它决定
pub const WIDGET_ITEM_GAP: f32 = 4.0;
/// 闪烁判定窗口：最后一次"灭帧"后多久内还算"正在闪烁"（毫秒）。
/// 微信亮/灭各约 250ms，留 2.5s 宽限，闪烁一停图标就从胶囊里收走。
pub const FLASH_WINDOW_MS: u64 = 2500;
/// 天气目标城市（留空 = 按出口 IP 自动定位）
pub const WEATHER_CITY: &str = "";
// 高德 Web 服务 key 不在源码里：填在 `data/settings.txt` 的 `amap_key=` 一行
// （设置页「顶部胶囊 → 天气」也能改）。留空则只用 Open-Meteo。
// 免费额度 5000 次/月，每次刷新消耗 2 次（实况 + 预报），30 分钟一次约 2900 次/月。
/// 天气刷新间隔（秒）
pub const WEATHER_INTERVAL: u64 = 1800;

// ---------- 邮箱 ----------

/// 邮箱 widget 开关
pub const SHOW_MAIL: bool = true;
/// IMAP 服务器（QQ 邮箱）
pub const MAIL_HOST: &str = "imap.qq.com";
pub const MAIL_PORT: u16 = 993;
/// 账号与授权码的**占位默认值**，恒为空。
///
/// **不要在这里填真账号**：这是源码常量，一旦提交就进了 git 历史。
/// 首次运行会用这两个空值生成 `<exe>/data/mail.txt`，请在那份文件里填
/// （或直接用设置页「顶部胶囊 → 邮件」添加账号，写进 `data/settings.txt`）。
pub const MAIL_USER: &str = "";
pub const MAIL_PASS: &str = "";
/// 拉取间隔（秒）
pub const MAIL_INTERVAL: u64 = 90;
/// 每次拉取的邮件条数
pub const MAIL_FETCH: usize = 6;

// ---------- 展开面板 ----------

/// 内边距
pub const PANEL_PAD: f32 = 12.0;
/// 标题区高度（含副标题）
pub const PANEL_HEAD_H: f32 = 48.0;
/// 每行高度
pub const PANEL_ROW_H: f32 = 40.0;
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
pub const PANEL_ICON_GAP: f32 = 7.0;
/// 标题与右侧数值的间距
pub const PANEL_VALUE_GAP: f32 = 12.0;

// ---------- 天气面板（宽卡片，左当前 / 右四天）----------

pub const WEATHER_PANEL_W: f32 = 520.0;
/// 卡片高度基准（实际高度取 `WEATHER_PANEL_H * 0.88 * 字号倍率`，左栏更高时再撑高）
pub const WEATHER_PANEL_H: f32 = 172.0;
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
/// Dock 图标边长（任务与托盘共用；比槽位小一圈，图标"占面"更透气，
/// Dock 总尺寸由槽位决定、保持不变）
pub const DOCK_ICON: f32 = 36.0;
/// 图标槽位（间距节奏，决定 Dock 总长度）
pub const DOCK_SLOT: f32 = 46.0;
/// 相邻图标间距
pub const DOCK_GAP: f32 = 6.0;
/// 左右内边距（比图标间隙略大，图标才不显得顶到边；也要盖过胶囊端部的圆弧）
pub const DOCK_PAD_X: f32 = 12.0;
/// 圆角半径：等于一半高度 → 完全胶囊形（高度变小时按 `min(dock_radius, 高/2)` 收）
///
/// 实际值随主题变（`cfg::cur().dock_radius`），这里只保留名字方便检索。
/// 波浪放大：光标划过时图标按与光标的距离高斯放大，最大 +45%
pub const DOCK_WAVE_AMP: f32 = 0.45;
/// 波浪放大的衰减半径（逻辑点）：约 1.3 个槽位，波浪覆盖相邻 2~3 个图标
pub const DOCK_WAVE_SIGMA: f32 = 62.0;
/// 距屏幕底部距离
pub const DOCK_BOTTOM_MARGIN: i32 = 8;
/// 运行中窗口重扫间隔（毫秒）
pub const DOCK_POLL_MS: u32 = 2000;
/// Dock 托盘区：图标边长。**与任务图标完全一致**，底下一排图标因此等大；
/// 源像素只有 16~24px，铺满 44 点靠 Catmull-Rom 放大（见 `tray::resize_premul`），
/// 个别只注册小 `hIcon` 的程序会略软，这是"一样大"的代价
pub const DOCK_TRAY_ICON: f32 = DOCK_ICON;
/// Dock 托盘区：槽位（= 任务图标槽位，悬停圆底大小才对得上）
pub const DOCK_TRAY_SLOT: f32 = DOCK_SLOT;
/// Dock 托盘区：图标间距
pub const DOCK_TRAY_GAP: f32 = DOCK_GAP;
/// 前台应用指示条厚度（逻辑点）。长度按槽位的 30% 算，见 `dockview`
pub const DOCK_ACTIVE_H: f32 = 3.0;

// ---------- Dock：同程序多开时的窗口选择弹窗 ----------

/// 每行高度
pub const WINLIST_ROW_H: f32 = 30.0;
/// 弹窗内边距
pub const WINLIST_PAD: f32 = 6.0;
/// 最小宽度（标题都短时别缩成一条）
pub const WINLIST_MIN_W: f32 = 190.0;
/// 最大宽度（标题超长按省略号截断）
pub const WINLIST_MAX_W: f32 = 340.0;
/// 行文字字号
pub const WINLIST_FONT: f32 = 12.5;
/// 弹窗与 Dock 图标的间距
pub const WINLIST_GAP: i32 = 6;
/// 鼠标离开弹窗多久后自动收起（毫秒）
pub const WINLIST_GRACE_MS: u128 = 450;

// ---------- Dock：返回桌面（右端细竖条）----------

/// 返回桌面开关（右端那条短竖条，点一下显示/还原桌面）
pub const SHOW_DESKTOP_BTN: bool = true;
/// 细竖条宽度（垂直于 Dock 主轴）
pub const DOCK_DESKTOP_W: f32 = 7.0;
/// 细竖条与图标区之间的间距。左侧按同样的宽度留一块空白占位，
/// 图标组才仍然是屏幕居中（右侧多宽、左侧就空多宽）
pub const DOCK_DESKTOP_GAP: f32 = 10.0;
/// 细竖条长度占 Dock 厚度的比例（留足上下呼吸，不顶满）
pub const DOCK_DESKTOP_H: f32 = 0.62;

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
/// 托盘表"瞬时为空"的宽限期（毫秒）：应用重注册图标时 `NOTIFYICONDATA` 表会空一瞬，
/// 这期间沿用上一批托盘槽位，别让 Dock 的尺寸跟着塌下去再长回来
pub const TRAY_EMPTY_HOLD_MS: u128 = 1500;
/// 把"鼠标停在 Dock 的托盘图标上"转发成真实图标的悬停：向图标的宿主窗口投它注册时的
/// 回调消息（wParam/lParam 按 uVersion 0/3/4 编码，坐标用图标矩形中心），**不移动光标**。
/// 回调消息号系统没有 API 可查，靠 `traymsg` 从目标进程内存里挖 `NOTIFYICONDATAW`，
/// 结果按 exe 缓存进 `orbit.traycache`（每个程序只需扫一次）。
pub const TRAY_FORWARD_HOVER: bool = true;
/// 伪造托盘点击/悬停（traymsg）：向目标窗口投它注册时的回调消息。
/// 对齐 Atrium 的「透明转发」后可关掉这条做 A/B：关闭时 Dock 托盘图标仍显示，
/// 但点击/悬停不再伪造投消息（完全交给已转发的 Explorer 托盘处理）。
pub const TRAY_FAKE_INPUT: bool = true;

/// 悬停转发后，应用自己弹的预览小窗（微信那种卡片）会落在**旧托盘角**。
/// 打开这个就把那个 tool window 摆到 Dock 正上方，并在它开着的时候一直守着别再漂回去。
/// 移植自 C# 版 Orbit 的 `HoverFlyoutRelocator`，实现见 `src/flyout.rs`。
pub const TRAY_RELOCATE_FLYOUT: bool = true;

/// 右键托盘图标后，应用弹的原生菜单（#32768）就落在注入坐标（= Dock 图标上），
/// 而 Dock 是 topmost，菜单会被压住一部分。打开这个就在菜单出现时把它挪到
/// 紧贴 Dock 边缘的位置（跟随鼠标方向），实现见 `src/flyout.rs::watch_menu`。
pub const TRAY_RELOCATE_MENU: bool = true;

// ---------- 悬停提示条（托盘/任务图标 tooltip）----------

/// 提示条字号
pub const TIP_TEXT_SIZE: f32 = 12.5;
/// 提示条左右内边距
pub const TIP_PAD_X: f32 = 10.0;
/// 提示条上下内边距
pub const TIP_PAD_Y: f32 = 5.0;
/// 提示条与 Dock 的间距
pub const TIP_GAP: i32 = 6;
/// 提示条最大宽度（超出按省略号截断）
pub const TIP_MAX_W: f32 = 260.0;

// ---------- 音乐条（旋转封面 + 歌词）----------

/// 歌词字号（逻辑单位；可由 settings.txt 的 lyric_size 覆盖）
/// 歌词字号默认与时间/日期字号一致（TIME_SIZE）
pub const LYRIC_FONT_SIZE: f32 = TIME_SIZE;
/// 音乐条左右内边距
pub const LYRIC_PAD_X: f32 = 16.0;
/// 音乐条与胶囊之间的间距（像素；用户要求左右距离再拉开一点，8 → 14）
pub const LYRIC_GAP: i32 = 14;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_keeps_rgb_and_forces_alpha() {
        assert_eq!(opaque(0xC0_16_18_1E), 0xFF_16_18_1E);
        assert_eq!(opaque(0xFF_12_34_56), 0xFF_12_34_56);
        assert_eq!(opaque(0x00_FF_FF_FF), 0xFF_FF_FF_FF);
    }
}
