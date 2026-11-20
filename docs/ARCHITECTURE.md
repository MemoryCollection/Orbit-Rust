# Orbit 架构总览

悬浮任务栏：顶部胶囊（时钟/日期 + 可分离出去的模块条 + 展开面板）+ 胶囊旁的音乐条（旋转封面 + 歌词）+ 底部 Dock（钉选与运行中应用）浮在所有窗口上方，不占用工作区。目标 Win11 23H2 (22631)、主屏、低内存（原生 Win32 + egui 软件后端，无 WebView）。**UI 绘制已全部迁到 egui**：各窗口用 egui 立即模式布局 → epaint 网格 → 公共软件光栅化（`egui_soft`）→ 预乘 RGBA → `render::present()`（UpdateLayeredWindow）；`render.rs` 只剩图标捕获/缩放、封面旋转、文本测量等非绘制能力。胶囊支持整体透明度与三种形状（悬浮 / 吸顶刘海 / 吸底），Dock 支持下/上/左/右四个锚位。

## 模块关系

```
main.rs → app.rs（启动/消息循环）
            ├─ window.rs  顶部胶囊（WS_POPUP + LAYERED + TOPMOST + NOACTIVATE）
            │    ├─ pillui.rs  胶囊 egui 布局（时钟+widget+闪烁图标）→ 公共软件后端
            │    ├─ weather.rs 后台线程拉高德（回退 Open-Meteo）
            │    ├─ mail.rs    后台线程 IMAP 拉邮件 + 验证码抽取
            │    ├─ music.rs   后台 MTA 线程采样 SMTC（曲目 + 播放头 + 专辑封面）
            │    ├─ lyric.rs   按曲目搜网易云取 LRC，按播放进度取当前句
            │    ├─ musicbar.rs 音乐条窗口壳（音乐条内容由 musicbarview 画）
            │    ├─ widgetbar.rs 分离模块条窗口壳（每侧最多 3 条，同侧同编号合并）
            │    ├─ clipboard.rs 后台线程监听剪贴板
            │    ├─ clock.rs   时钟模块（日期 + 时间合并成一段，格式由设置决定）
            │    ├─ panel.rs   展开面板窗口壳（点 widget 弹出，回传动作号）
            │    └─ tray.rs    托盘镜像数据层（优先消费 trayhost 托管表，UIA+PrintWindow 兜底）
            ├─ dock.rs    底部 Dock（同款分层窗口，独立 wndproc + 2s 重扫定时器 + 悬停提示条 ORBIT_TIP）
            ├─ egui_soft.rs   公共 egui 软件后端：Context/纹理/字体 + epaint 网格→预乘 RGBA 光栅化
            ├─ dockview.rs    Dock + 提示条的 egui 绘制
            ├─ widgetbarview.rs 分离模块条的 egui 绘制
            ├─ musicbarview.rs  音乐条的 egui 绘制（封面旋转仍用 render::rotate_cover）
            ├─ panelview.rs  展开面板（列表+天气卡片）的 egui 绘制
            ├─ render.rs  仅剩：图标/封面捕获与缩放、封面旋转、文本测量、present() 上屏（无 UI 绘制）
            ├─ trayhost.rs 完整托盘托管：假 Shell_TrayWnd + TaskbarCreated + WM_COPYDATA 图标表
            ├─ traymsg.rs 托盘回调消息转发（按 v0/3/4 编码，不动光标）
            ├─ flyout.rs  悬停预览卡片重定位（搬到 Dock/胶囊的上/下方并守位）
            ├─ store.rs   唯一持久化入口（全部落在 <exe>/data/）
            ├─ settings.rs 可调设置（data/settings.txt，改文件即时生效；set_* 写盘 + 版本号）
            ├─ settingsui.rs 设置页面（egui/eframe 独立小窗，immediate mode 控件，改值即生效）
            ├─ trace.rs   日志（<exe>/orbit.log，windows 子系统下没有控制台）
            └─ hide.rs    原生任务栏真隐藏（ABM_SETSTATE(AUTOHIDE) + SWP_HIDEWINDOW）
```

所有分层窗口共用同一条绘制链路：**egui 立即模式布局（逻辑点）→ `egui_soft` 公共软件光栅化 → 预乘 RGBA → `render::present()`（UpdateLayeredWindow）**。这是本项目的重要设计约束——这些窗口依赖 NOACTIVATE + 逐像素 alpha + 圆角命中，不能换成 eframe 原生窗口（普通窗口不支持逐像素透明）。egui 后端按窗口分布：胶囊=`pillui`（window.rs 持 `PillUi`）、Dock+提示条=`DockView`（dock.rs 持字段）、模块条=`SoftBackend`（widgetbar 每窗口 thread_local 一份）、音乐条=`SoftBackend`（musicbar thread_local）、面板=`SoftBackend`（Panel 持字段）。各窗口仍持有自己的状态结构（`window::App` / `dock::Dock` / `panel::Panel`），通过窗口 `GWLP_USERDATA` 取回；音乐条/模块条/提示条仍无独立状态、内容几何由胶囊按需传入。设置页（`settingsui`）是唯一例外：egui/eframe 小窗跑在独立线程里，自己接鼠标键盘、自己写设置，谁也不通知——胶囊与 Dock 靠比对 `settings::version()` 发现变化后重绘。

> 常见坑（egui 软件后端）：① 输出是**预乘 RGBA**，整体透明度要连 RGB 一起乘，再换 BGRA 上屏（`premultiplied_rgba_to_bgra`）；② 命中矩形要在逻辑点收集后乘 scale 转物理像素；③ 图标/封面像素块先 `SoftBackend::soft_image()` 上传成 egui 纹理再 `p.image()`；④ 变化检测（hash）必须保留，否则定时器空转光栅化；⑤ 中文字体靠 `install_fonts` 读系统微软雅黑注入 egui 字体族。

## 关键设计

### 悬浮窗口（三个窗口共同点）
- 不注册 AppBar（ABM_NEW），所以不占用工作区，浏览器可全屏。
- 置顶 + `WS_EX_NOACTIVATE`（点击不抢焦点）；圆角外区域 `WM_NCHITTEST → HTTRANSPARENT` 点击穿透。
- 每帧用 egui 布局 → epaint 网格 → `egui_soft` 软件光栅化成预乘 RGBA → premultiplied BGRA → `UpdateLayeredWindow`，透明度烘焙进像素，改透明度只需重绘。
- 注册窗口类时必须给 `hCursor`（`LoadCursorW(None, IDC_ARROW)`），否则系统会套用"程序启动中"的转圈光标。
- **位置固定**：胶囊贴主屏顶部居中，Dock 贴主屏底部居中，均不可拖动。两者的**水平居中都按 `rcMonitor` 计算**（同一基准）；垂直方向任务栏还活着时 Dock 用 `rcWork`（落在任务栏上方），任务栏被接管后用 `rcMonitor.bottom`（贴屏幕底）。
- **没有控制台**：`main.rs` 里 `#![windows_subsystem = "windows"]`，双击 exe 不会弹 cmd 窗口；所有诊断输出改走 `trace.rs` 写 `<exe>/orbit.log`。

### 胶囊 widget 机制
胶囊内容是"时间 + 日期 + 若干 widget"（**托盘图标在底部 Dock 右侧，不在胶囊里**）。widget 采用**数据与绘制分离**的轻量设计（不是 trait 对象）：

1. 各数据模块（weather / clipboard / mail）自己管数据与后台线程，暴露 `tick() -> bool`（有新数据就返回 true）。
2. `window::App::build_widgets()` 把它们翻译成 `render::WidgetItem { icon, icon_argb, text }`，同时记录 `WidgetKind` 决定点击行为，并按模块布局分成「胶囊里的 / 左条的 / 右条的」三份。
3. `pillui::paint()` 用 egui 统一排版并返回每个 widget 的命中矩形（逻辑点 × scale 转物理像素）；分离出去的那些走 `widgetbarview::paint()`（同一套 widget 排布代码，文本按**垂直中线对齐**）。
4. 点击按 `WidgetKind` 分发（胶囊内直接命中；模块条经 `WM_WIDGET_CLICK` 带类型号回传）。无数据的 widget 不占位（不会留空白）。

**模块布局 = 排序列表 + 每模块模式**（取代旧的「左1/左2/左3/右1/右2/右3」编码）：`data/settings.txt` 里 `module_order` 是逗号分隔的模块名排序列表（`time,weather,mail,clipboard`，时间日期固定第一不可移动），`weather_mode` / `mail_mode` / `clipboard_mode` 各取 `pill`（留在胶囊里）/ `left` / `right` / `off`（隐藏）。规则：**同侧的模块合并成同一条**，同侧第几个就排第几条（上限 `WIDGET_BAR_MAX_SLOT = 3`，与预建窗口数一致）；`off` 的模块直接丢弃（胶囊/条里都没有）。设置页里这一行是**芯片（Chip）列表**：时间日期芯片固定画在最前（不可拖不可点），其余芯片**可拖动排序**（按下后 SetCapture，拖过相邻芯片中心就交换并即时写 `module_order`）、点击循环切换状态（胶囊 → 左条 → 右条 → 隐藏）。所有模块（含时间日期之外的全部）都能这样隐藏/显示。分离条与胶囊**同高（`PILL_H`）、同底色、同圆角**，条与条、条与胶囊之间都留 `WIDGET_BAR_GAP`；没有内容的那条自动隐藏。分离条窗口带整体透明度（与胶囊 `pill_alpha` 一致，烘焙进像素）。后期新模块只要往 `build_widgets` 的列表里加一项、配一个 `xxx_mode` 键并在 `module_order`/`MODULES` 里登记，就自动获得同样的分离/分组/隐藏能力。

> 「鼠标在不在胶囊里」的判断因此要把模块条算进去（`widgetbar::contains`），否则点开面板后鼠标停在条上会被误收起。

**图标统一视觉尺寸**：胶囊里的图标都是 `Segoe UI Symbol` 字形，但同一字号下墨迹大小差很多（`☁` 比 `✉`/`▤` 大一圈）。所以图标不是直接 `DrawString`，而是先把字形渲染成 alpha 蒙版（`render::build_mask`，按字形+字号缓存），量出墨迹包围盒，再按**墨迹面积**缩放到 `WIDGET_GLYPH_INK`、居中贴进固定 `WIDGET_ICON_CELL` 的格子里。收益：换任何字形观感都一致，格子宽度固定 → 间距均匀、胶囊宽度稳定；贴图在像素取出后对 RGBA 直接做软合成 + 超采样，缩小时不糊。

**日期与时间是同一个模块**（用户要求合并）：`clock.rs` 把两者拼成**一段**文本，`pillui` 只画一次、只量一次宽（egui 字体度量），整段同字号同颜色（`TIME_SIZE = 20`，egui 比例字体 + 系统微软雅黑/符号字体回退），和后面的 widget 区之间才画分隔线。格式是 **strftime 风格的格式串**（`data/settings.txt` 的 `clock_format`，默认 `%Y-%m-%d %H:%M:%S`）：支持 `%Y` 四位年、`%y` 两位年、`%m` 月、`%d` 日、`%H` 24 时、`%I` 12 时、`%M` 分、`%S` 秒、`%p` 上午/下午、`%a` 周五、`%A` 星期五、`%%` 百分号，用户可自由组合任意样式；设置页里是一行**单行文本编辑框**（点进去直接打字、左右/Home/End 移光标、回车/失焦提交）+ 下方**实时预览**行（`clock::sample`，设置页开 1s 定时器重画，秒跟着走）。带 `%S` 的格式下"变化比较"会把秒算进去，秒一跳就重绘；不带的仍然只在分钟变化时重绘。老版本的 `clock_order`/`time_format`/`date_format` 三键在读取时自动迁移成等价格式串。

**胶囊的水平居中只按胶囊自己**（`w = content_width(...)`，`rcMonitor` 取中）：分离出去的模块条、音乐条都不参与居中，条只按胶囊往外排（撞到屏幕边才内收）。

**托盘闪烁图标（新消息提醒）**：应用用托盘图标一闪一闪提示新消息（微信就是这样）。Dock 的 70ms 采样本来就在跟真实闪烁，于是它把"正在闪烁的图标"发布成快照（`dock::FlashIcon`：稳定键、像素、亮/灭帧、回调目标 `Raw`、`flash_version()`）。胶囊开一个 70ms 的 `T_FLASH` 定时器**只比版本号**，变了才重绘：在最右侧按 widget 区同款分隔线 + 固定格子画出这些图标，**灭帧留空位不画**（宽度不抖，视觉上就是"图标-消失-图标"与托盘同频）。闪烁一停（最后一次灭帧超过 `FLASH_WINDOW_MS = 2.5s`）图标自动收走。点它走 `traymsg::click`，和点 Dock 上的托盘图标是同一个效果（不移动光标）。

> 音乐不在胶囊里：曲目/歌词/封面合成一条独立的"音乐条"贴在胶囊旁边（见下），没有播放时整条隐藏，所以胶囊宽度不会被歌名长短拖着变。

当前的 widget 与交互：

| widget | 内容 | 左键 | 右键 |
|---|---|---|---|
| 天气 | `☀ 4°`（符号 + 整数温度，晴天金色/夜间淡蓝/雨雪冷蓝） | 展开天气卡片 | — |
| 邮箱 | 平时只有 `✉`；识别到验证码显示验证码（金色）；有未读但没验证码显示"新邮件"（强调色） | 有验证码 → 复制并收回图标；否则展开邮件面板 | — |
| 剪贴板 | 只有 `▤`，**不显示内容** | 展开剪贴板面板 | — |

## 展开面板（panel.rs）

一个独立的 `ORBIT_PANEL` 分层窗口，胶囊下方 6px 处，水平方向：**天气卡片与胶囊中心对齐**（它比胶囊宽得多，跟着天气 widget 跑会歪），列表面板按被点 widget 居中（widget 被分离到左右条时，锚点用条上那个 widget 的中心）；左右自动收进工作区。

- **单向协议**：面板只负责绘制与命中，点击时 `PostMessage(owner, WM_ROW_CLICK, 动作号, 0)` 回胶囊窗口；是否复制/已读/固定/收起全由 `window::App::on_panel_click()` 决定。面板自己不含业务逻辑，也不会自己收起。
- **动作号编码**（`render.rs`）：`0..999` = 行号；`1000+行号` = 该行的固定按钮；`2000+序号` = 标签页。
- **命中表**：`PanelRender { rgba, hits: Vec<([x,y,w,h], 动作号)> }`；后压入的区域优先（固定按钮压在行上面），因此命中时**倒序**查找。
- **两种形态**：通用列表（剪贴板 / 邮件，可带右上标签页）与天气宽卡片（`Content::Weather`，只有"刷新"一个热点）。
- **天气卡片排版**：左栏实况分三层——① 图标 + 大号温度同一行；② 天气 · 城市（城市小一号、压暗）；③ 详情（最多两行）+ 数据来源。纵向节奏全部走 `LEFT_*` 常量（`render.rs`），乘同一个字号倍率，所以放大字号不会把间距压乱；右栏四天预报的基准与左栏首行墨迹对齐。卡片高度由 `weather_panel_size(scale, &view)` 统一算，左栏行数多或字号大就自动撑高，绝不裁字。
- **自动收起**：胶囊每 500ms 检查光标是否还在"胶囊 ∪ 面板"内，连续 2 次（约 1 秒）在外部就收起。
- **实时刷新**：`weather/mail/clipboard.tick()` 报告数据变化时，若对应面板正展开则原地重建内容（不闪、不重置位置）。

## 音乐条（music.rs + lyric.rs + musicbar.rs）

胶囊旁边的一条独立小窗，把"音乐控制"和"歌词"合成一个：**左边是旋转的圆形专辑封面（中心是播放/暂停按钮），右边是当前这一句歌词**（控制在前、歌词在后），默认贴在胶囊右侧。没有歌曲播放时整条隐藏，不留空占位。

- **窗口**：`ORBIT_MUSICBAR`，同款分层窗口（TOP/LAYERED/NOACTIVATE/TOOLWINDOW），没有 state、没有状态机——`App::tick_bar()` 每 200ms 算一次内容和几何，`musicbar::show()` 只在"文本 / 角度 / 播放态 / 悬停态 / 封面 / 位置"变了才重画。40ms 的旋转定时器只在播放时开着，暂停就 `KillTimer`，不空转。
- **命中**：整窗默认 `WM_NCHITTEST → HTTRANSPARENT`（文字区域鼠标完全穿透），只有封面那个圆返回 `HTCLIENT`。左键 = 播放/暂停，右键 = 下一首；点击不自己处理业务，`PostMessage(胶囊, WM_TOGGLE/WM_NEXT)` 由 `App` 分发到 SMTC。
- **封面**：`MediaProperties.Thumbnail`（`IRandomAccessStreamReference`）在**换曲时**读一次，读成原始图片字节随采样送回主线程；主线程用 GDI+（`SHCreateMemStream` + `GdipCreateBitmapFromStream`）解码 → 中心裁成正方形 → 面积平均缩到封面尺寸。没有封面（或解码失败）就画暗色圆 + 音符占位，失败结果按曲目记住，不重试。
- **旋转**：`render::rotate_cover()` 逐像素逆旋转采样（45°/s，约 8 秒一圈），边缘 1px 抗锯齿，圆外全透明；暂停时角度定格，换曲归零。中心按钮的 ▶/⏸ 用图形直接画（34px 里用字体排版会偏一两像素）。
- **位置**：`settings.txt` 的 `lyric_position` = `right`（默认）/ `left` / `bottom`；宽度按该侧可用空间截断，再用省略号，绝不越出屏幕。胶囊宽度变化后 `App::render()` 会顺手重贴一次。
- **歌词**：换歌时（歌名或歌手变化）起一个短命线程，先按"歌名 + 歌手"搜网易云，再取 `lrc.lyric`；打分挑歌（歌名完全一致 +4、互相包含 +2，歌手命中 +3，没有歌名匹配就不算候选）。解析 LRC（支持一行多时间标签），按时间排序去重。
- **同步**：`lyrics.line_text(pos_ms)` 取"最后一个起始时间 ≤ 当前进度"的那句；前奏、没取到词、整首没有歌词时都退回显示"歌名 · 歌手"（条本身不隐藏，因为还有控件）。
- **缓存**：`Lyrics` 内部按曲目键缓存最近 16 首的结果，**包括"这首歌没有歌词"**，所以来回切歌不会反复打接口。

### 数据来源（参考 xiaotang-daozai 的实现选型）
- **天气**：数据源由设置页选（`weather_source` = `amap` / `openmeteo`），高德 key 可在设置页填（`amap_key` 覆盖编译期默认）。默认源高德 Web 服务（`/v3/ip` 定位 → `/v3/weather/weatherInfo?extensions=all` 实况 + 四天预报；免费额度 5000 次/月，30 分钟刷一次约 2900 次）。`/v3/ip` 拿不到 `adcode` 时退到 `/v3/geocode/geo`（用 IP 城市名反查）；高德整体失败（或 key 为空）则回退 Open-Meteo（免 key，`current` + `daily`，`ipwho.is` 定位）。指定 Open-Meteo 时直接走高德失败回落以外的路径、不请求高德。定位缓存 3 小时、adcode 24 小时，失败 120s/240s/… 退避到 30 分钟。
- **邮箱**：`native-tls`(schannel) 上的最小 IMAP4rev1 子集（LOGIN/SELECT/FETCH/STORE/LOGOUT），`UID FETCH n:m (UID FLAGS BODY.PEEK[HEADER.FIELDS(...)] BODY.PEEK[TEXT]<0.4096>)`；MIME 头 `=?UTF-8?B?..?=` 解码、base64 / quoted-printable / GBK 正文还原，再从"验证码/verification code"附近抓 4~8 位数字（兜底独立 6 位）。90 秒拉一次最近 6 封。**账号来源**：`data/mail.txt` 为基础，设置页写入的 `mail_host/port/user/pass` 逐字段覆盖（留空 = 回落文件值），方便不改文件换账号。
- **邮件已读**：双向。拉取时读服务端 `FLAGS` 里的 `\Seen`（别的客户端读过也算已读）；用户点开邮件时由常驻后台线程 `UID STORE <uid> +FLAGS (\Seen)` 写回服务器，本地再留一份 UID 记录做离线兜底。STORE 失败只丢连接、下次点击重连，不重试轰炸服务器。
- **邮件阅读窗**（`mailview.rs`）：点邮件标题在面板右侧弹出普通 popup 窗（UpdateLayeredWindow 分层窗带不了可见子控件），只读多行 EDIT 呈现正文——文本可选中、可 Ctrl+C、可滚动；「删除邮件」按钮经 `WM_COPYDATA`(dwData=`0x4D41494C` 魔数) 把 UID 同步发回胶囊窗口，由 `Mail::delete` 本地移除 + 后台线程 `UID STORE \Deleted` + `EXPUNGE` 真删服务器邮件，删除后 800ms 自动重拉列表。
- **邮件解码**：`decode_words` 处理 RFC2047 编码字（多段相邻编码字按 `i += end+2` 跳转）；裸 QP 主题（无 `=?charset?` 包装，直接 `=XX=XX` 序列）在 `clean_subject` 里按字节解码兜底；`strip_tags` 先整块删除 `<style>`/`<script>` 内容再剥标签，正文不会混入 CSS。
- **音乐**：WinRT `GlobalSystemMediaTransportControlsSessionManager`（`windows` crate 的 `Media_Control` feature）。独立 MTA 线程每 250ms 采样，通过 channel 把 `Sample { now, pos_ms, art }` 送回主线程；控制命令走反向 channel。没有播放器时整条音乐条隐藏。
- **专辑封面**：同一次采样里拿 `MediaProperties.Thumbnail`，换曲时 `OpenReadAsync` → `DataReader` 读成字节（上限 8MB），PNG/JPEG 都行；实测 QQ 音乐/网易云这类播放器给的是 PNG。
- **播放进度（歌词对齐的关键）**：部分播放器（实测 lx-music-desktop）只在换曲/暂停时写一次 `Position`，正常播放时它是冻结的（如 `Position=12ms`、`LastUpdatedTime` 也停在那一刻）。所以线程里维护一个**播放头**：曲目切换时 `playhead = Position + (现在 - LastUpdatedTime)`（把已经播过的一段补回来），之后"Position 变了就对锚，没变就按本地时钟 +dt"；暂停时不推进。主线程 `position_ms()` 再按"上次采样时刻"外推，两次采样之间也能对齐到 200ms 精度。实测与 LRC 时间戳误差在 0.1s 内。
- **剪贴板**：`AddClipboardFormatListener` + 消息窗口（独立线程，零轮询）。**只收纯文字**：带 `CF_HDROP`（复制文件）直接跳过；带图片格式（`CF_DIB`/`CF_DIBV5`/`CF_BITMAP`/`PNG`）时，只有同时存在 `Rich Text Format` 或 `Csv` 才收（Word/Excel 复制文字会附带预览位图，纯图片没有这些文本格式）。此外按内容去重（重复内容移到最前）、固定条目永远排在最前，并跳过 `ExcludeClipboardContentFromMonitorProcessing` / `Clipboard Viewer Ignore` / `CanIncludeInClipboardHistory=0` 的敏感内容。
- **HTTP**：`net.rs` 直接用 WinHTTP（`WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY` 走系统代理），不引入 reqwest/ureq；JSON 用自写的 `json.rs`，两者都为零依赖。

### 数据持久化（store.rs）
**全部写在 exe 同目录的 `data/` 下**，不写注册表、不碰安装目录之外的任何位置（便携式）。

| 文件 | 内容 |
|---|---|
| `data/mail.txt` | IMAP 账号（host/port/user/pass 授权码），首次运行由 `config.rs` 默认值生成 |
| `data/mail_read.txt` | 已读邮件的 IMAP UID，一行一个（服务器 `\Seen` 的本地兜底） |
| `data/clipboard.txt` | 剪贴板历史，每行 `时间 \x1f 固定标记 \x1f 文本`（换行转义 `\x1e`；老格式两字段按未固定读入） |
| `data/settings.txt` | 可调设置，`key=value` 一行一个：`clock_format`（strftime 格式串，默认 `%Y-%m-%d %H:%M:%S`）、`module_order`（模块排序列表）、`weather_mode` / `mail_mode` / `clipboard_mode`（pill/left/right/off）、`pill_alpha`（0.2~1.0）、`pill_anchor`（float/island/bottom）、`dock_anchor`（bottom/top/left/right）、`weather_text_scale`（0.8~1.8，默认 1.25）、`lyric_position`（right/left/bottom）、`lyric_size`（10~26，默认 14）、`weather_source`（amap/openmeteo）、`amap_key`（设置页写入的高德 key 覆盖）、`mail_host` / `mail_port` / `mail_user` / `mail_pass`（设置页写入的邮箱账号覆盖，留空回落 `mail.txt`）、`show_tray` / `show_desktop_btn`（1/0）、`hide_tray`（要单独隐藏的托盘图标键，`;` 分隔）。老键 `clock_order` / `time_format` / `date_format` / `*_pos` 在读取时自动迁移 |

### 可调设置（settings.rs）
- 文件不存在时写入带注释的默认值；老版本文件缺新键会自动补到末尾（`append_missing`），方便直接编辑。
- 启动时读一次，之后每 500ms 比一次 mtime，改了就热更新并重绘（胶囊、展开中的面板、音乐条、左右模块条都会跟着变）。
- 天气卡片的字号、行距、卡片宽高都乘 `weather_text_scale`，所以调大字号不会把文字挤出卡片；卡片高度还会按左栏实际行数撑高。
- 音乐条按 `lyric_position` 决定贴哪一侧、按 `lyric_size` 决定歌词字号（宽度随文本自适应，超出按侧边可用空间截断）。
- 模块位置按 `weather_pos` / `mail_pos` / `clipboard_pos`：`pill` 留在胶囊里，`left[2|3]` / `right[2|3]` 分离成胶囊旁的模块条并按编号分组（同侧同编号合并成一条）。**默认全部分离且一个模块一条**（天气左1、邮件右1、剪贴板右2），胶囊里只剩时间和日期。
- `show_tray` 控制 Dock 是否显示托盘图标，`show_desktop_btn` 控制 Dock 右端那条"返回桌面"细长条。
- `hide_tray` 是"在 Dock 上单独隐藏某个托盘图标"的名单（`;` 分隔）。键由 `tray::TrayItem::key` 生成，**跨会话稳定**：GUID 注册的图标用 GUID，否则用 `exe 全路径#uID`（hWnd 每次启动都变、tooltip 会随状态变，都不能当键）。`Dock::refresh_tray_shown` 在"与 Dock 图标去重"之后再过一遍这份名单，所以被隐藏的图标既不占槽位也不参与悬停/点击；它仍然注册在托盘托管表里，设置页随时能恢复。
- **两份读取路径共用一套值**：手改文件走 mtime 轮询（`reload_if_changed`，胶囊 500ms 一次，Dock 另开 500ms 定时器 `T_SETTINGS`）；设置页面改值走 `set_xxx()`（更新原子量 + 写文件 + `VERSION += 1`），两边都靠 `settings::version()` 判断"我该重绘了"。这样多个窗口不再争抢"谁先看到 mtime 变化"（争抢会让后到的窗口永远看不到变化）。
- `save()` 只替换**它管的键**的值，注释、空行、用户自己加的其它键原样保留。

### 胶囊形状与透明度
- **`pill_alpha`（0.2~1.0）**：胶囊、分离模块条整体透明度。渲染完 RGBA 后逐像素把 alpha 乘系数再上屏（premultiplied 下只乘 alpha 通道即可），改透明度只是重绘，不另走系统接口。
- **`pill_anchor`（float / island / bottom）**：胶囊的形状与吸附位置。`float` = 悬浮（四角全圆角，距屏顶 `TOP_MARGIN`）；`island` = 吸顶刘海（**顶部两角直角**贴住屏幕上沿，下面圆角）；`bottom` = 吸底（底部两角直角贴屏幕下沿）。渲染端 `PillShape` 决定 `capsule_path` 的顶部/底部圆角半径，命中测试 `inside_pill` 按同款形状判断（刘海的直角区域也算命中）。
- **吸附位置的连锁反应**：胶囊吸底时，展开面板自动弹到胶囊**上方**（`panel_top_y` 按 `panel::content_size` 预算高度），托盘悬停卡片也改守位到胶囊**下方**（`flyout::watch` 的 `below = true`），保证任何形状下卡片都不出屏。

### Dock 锚位
**底部 Dock 的命名**：窗口类名 `ORBIT_DOCK`（`src/dock.rs`，结构体 `Dock`），中文叫"底部 Dock"。`dock_anchor` 支持 `bottom`（默认，贴工作区下沿居中）/ `top`（贴主屏上沿）/ `left` / `right`（左右两侧竖排居中）：竖排模式 `dockview::dock_size` 返回 (宽, 高) 互换、`dockview::paint` 用主轴抽象（`cross`/`long`）复用同一套布局，分隔线、悬停底、返回桌面细长条都跟着转向；Dock 在顶部时悬停卡片守位到 Dock 下方（`below`），提示条 `ORBIT_TIP` 也按锚位改到不遮挡的一侧。

### 设置页面（settingsui.rs）

**egui 版**：`eframe`（glow 后端）跑在一条独立后台线程里，主线程的 Win32 消息循环完全不受影响。窗口是无边框小圆角窗（`with_decorations(false)`、`with_taskbar(false)` 不占任务栏，不置顶，打开时 `Focus` 弹到最前），**固定尺寸 940×560**、打开时主屏居中（`OuterPosition` 按系统 DPI 把物理像素换算成逻辑点）。中文字体从系统加载（微软雅黑 → 黑体兜底）追加进 egui 字体族；配色对齐胶囊/面板（`PANEL_BG_ARGB` / `ACCENT_ARGB`）。

- **线程模型**：eframe 线程**懒启动**（首次 `show()` 才起，避免 eframe 第一帧后强制 `set_visible(true)` 导致设置页开机弹出）；主线程经 `QUEUE`（Mutex 队列）+ `egui::Context::request_repaint` 收发 显示/隐藏 命令，eframe 的 `App::logic`（窗口隐藏时也会被调用）负责消费命令、拦截 Alt+F4（当作收起，`CancelClose`，eframe 常驻不退出）。`is_open` 用 AtomicBool 同步回主线程。winit 事件循环经 `event_loop_builder` 钩子开了 `with_any_thread(true)`（非主线程建 EventLoop 必需）。
- **打开**：右键 Dock 空白处、右键胶囊、或 `Ctrl+Alt+,`（再按一次 / 点右上 ✕ / 点胶囊 widget 收起）。
- **布局（分组导航式）**：左栏是 6 个分组按钮（顶部胶囊 / Dock / 音乐条 / 天气 / 邮箱账户 / 模块与托盘），右栏只画选中分组的详细设置，内容超出窗口就组内滚动。
  - 顶部胶囊：时钟格式**编辑框** + 实时预览（500ms 跟走）、透明度加减、形状三段「悬浮/吸顶刘海/吸底」。
  - Dock：位置四段「下/上/左/右」。音乐条：位置三段 + 歌词字号加减。
  - 天气：字号倍率加减、数据源二段「高德/Open-Meteo」+ 高德 Key 编辑框。
  - 邮箱账户：**服务器预设**（QQ/163/Gmail/Outlook 四个 `MAIL_PRESETS`，按已存 host 自动高亮；「自定义」才展开 服务器/端口 编辑框），账号 + 授权码（`password(true)`）两个编辑框，**「测试连通」按钮**——后台线程跑 `mail::test_account`（TCP+TLS 建连 → IMAP LOGIN，复用 `mail.rs` 的 `Conn`），结果（成功/失败原因）回 UI 显示，测试期间按钮禁用并显示「测试中…」。
  - 模块与托盘：模块**芯片列表**（拖动排序 + 点击循环状态，芯片上标注当前去向「胶囊/左条/右条/隐藏」并用底色区分）+ 托盘图标两列开关（第一格固定「返回桌面」，其余逐个图标一个开关，另加"恢复全部显示"按钮）。
- **immediate mode**：不再有命中矩形表与自绘控件（旧 `render::SettingsCtl` 七件套已删除）；分段/加减器/开关都是 egui 小部件，交互状态 egui 自己管。编辑框用本地缓冲：**失焦才提交写盘**（单行框回车即失焦），打字过程零磁盘 I/O；失焦且 `settings::version()` 变化时才从 settings 重读，避免打字时被写盘值顶掉光标。注意 egui 的坑：`ScrollArea` 的内容 Ui 会**继承父布局方向**，放在 `ui.horizontal` 里必须用 `with_layout(top_down)` 显式掰回纵向。
- **托盘列表**：数据来自 `trayhost::table()` 的图标表（顺序与 Dock 一致），格子标签取 tooltip 首行（空则 exe 文件名，超 10 字截断加省略号）。
- **改值**：一律走 `settings::set_*`（更新原子量 + 就地改文件 + 版本号 +1），没有 IPC、没有中间状态；胶囊/Dock 在自己的定时器里比 `settings::version()` 重绘。芯片拖动即写 `module_order`；邮箱四字段聚合成一次 `set_mail_account`（点预设 = 写 host+port，账号授权码失焦时提交）。
- 编辑过的 `settings.txt` 只被替换受管键的值，注释和用户自己加的键都留着；写完同步 mtime，避免触发一次多余的重载。

### 底部 Dock
1. **内容** = 钉选应用（`%APPDATA%\...\User Pinned\TaskBar\*.lnk`）+ 运行中应用（`EnumWindows` 过滤），按 exe 去重。
2. **窗口过滤**：`IsWindowVisible`、非 `WS_EX_TOOLWINDOW`、无 owner、非 cloaked（`DWMWA_CLOAKED`）、有标题、排除自身窗口类。
3. **图标**：`SHGFI_SYSICONINDEX` → `SHGetImageList(SHIL_JUMBO)` 取 256px 图标 → `render::resize_rgba()` 面积平均缩到 Dock 尺寸。
4. **钉选项去重**：`.lnk` 目标路径（展开 `%windir%` 等变量）与运行 exe 路径比对；CLSID 快捷方式（如 File Explorer）无目标路径，退化为归一化名称包含匹配（仅长度 ≥ 8 时启用，避免"微信/企业微信"误合并）。
5. **点击**：钉选项 `ShellExecuteW("open", lnk)`；运行项 `IsIconic → SW_RESTORE` + `AttachThreadInput` 绕过前台锁后 `SetForegroundWindow`。
6. **托盘区**：应用图标之后接一条分隔线，再排托盘图标。托盘图标尺寸与任务图标**完全一致**（`DOCK_TRAY_ICON = DOCK_ICON` 44、`DOCK_TRAY_SLOT = DOCK_SLOT` 46、`DOCK_TRAY_GAP = DOCK_GAP` 6），悬停药丸也共用同一半径（`槽位/2 + 3`），这样底部就是一排等大的图标。托盘槽位单独一套 `tray_rects`，命中时**先查托盘再查应用**。
7. **右端"返回桌面"细长条**：Dock 最右侧一条 `DOCK_DESKTOP_W = 7`、高 `DOCK_DESKTOP_H = 0.62·DOCK_H` 的竖圆角条，悬停变亮、提示条写"返回桌面"，点击调 Shell 的 `IShellDispatch4::ToggleDesktop`（= Win+D，桌面图标一起收/展）。**左侧按同样宽度留空白占位**（`DOCK_DESKTOP_W + DOCK_DESKTOP_GAP`），否则右边多出来一块会把整排图标推离屏幕中心。命中区两侧各外扩 `DOCK_DESKTOP_GAP`（7 点宽根本点不准）。`show_desktop_btn = 0` 时两侧同时消失，Dock 宽度随之收窄。
8. **托盘轮询**：托盘镜像由 Dock 持有，跟应用重扫共用 2s 定时器（`T_RESCAN`）：`scan() | poll_tray()` 任一有变化就重绘。收到 `TaskbarCreated`（explorer 重启）时把镜像整个丢掉重建。
9. **鼠标转发**：左键直接调 `TrayMirror::click(idx, false)`（内部走 `DoDefaultAction`，之前先 `AllowSetForegroundWindow(ASFW_ANY)`——托盘动作常要弹窗口/菜单，没有前台权限会被焦点锁挡住，表现为"点了没反应"）；右键**必须等 `WM_RBUTTONUP` 再注入**——在 `WM_RBUTTONDOWN` 里注入的右键会和用户正按着的那次物理按键合成一次"按下-抬起"，真实托盘图标收不到点击（实测 traytest 无任何回调）。
10. **闪烁跟随**（`T_FLASH`，70ms）：应用用"托盘图标一闪一闪"提示新消息（微信就是这样，实测两个双开实例都在闪：约 250ms 亮 / 250ms 灭，灭帧是 `NIM_MODIFY(NIF_ICON)` 带 `hIcon=0`，亮帧是真实 `hIcon`）。托管模式下 `trayhost::apply()` 把 `hIcon=0` 标到 `Icon.dim`；渲染时**灭帧画全透明像素**（`blank = vec![0; px·px·4]`），亮帧画真实图标，即用户要的"图标-消失-图标-消失"真交替，与真托盘同频。关键：`poll_flash()` 在托管模式下**也调 `tray.poll()`**（早期版本直接 return false，导致 dim 只由 2s 重扫驱动而严重漏帧、卡在灭帧不恢复）；70ms 定时器足以跟上微信 200~300ms 一跳的帧。`poll_hosted()` 用 `host_rgba` 缓存按 (hwnd,uid,guid) 存最近一次抓到的像素，灭帧 hicon=0 或抓图偶发失败时沿用，保证亮帧一定能恢复出图标、不掉到 placeholder 暗点。兜底的 UIA 路径才用 `PrintWindow` 采样墨量判灭。
    **微信提示音**（`src/notify.rs`，设置键 `wechat_sound`，默认开）：微信系的托盘图标**从"没在闪"变成"在闪"**的那一下（`publish_flash` 里对比上一轮快照），Orbit 播一声嵌进二进制的提示音（`include_bytes!`，约 36 KB）——不改微信本体，微信自己的声音照旧。mp3 首播时释放到 `data/wechat_notify.mp3` 再走 MCI（`mciSendStringW`，DirectShow 后端免解码依赖），alias 打开一次复用、每次 seek 回头重播；想换音效用同名文件覆盖它即可。同一轮提醒只响一声：调用侧靠"新出现在闪烁集合"判定 + 3 秒最小间隔双保险，亮/灭交替帧不会连环轰炸。
11. **图标来源**：优先用程序 exe 的 256px 图标（`appicon`：注册表 `NotifyIconSettings` 的 `InitialTooltip` → `ExecutablePath`，进程名兜底），比从任务栏抠 16px 再放大清晰得多；但**必须先验相似度**——输入法这类"中/英状态"图标的 exe 图标完全不是一回事，`icon_diff()`（只比两边都不透明的像素）超过 `TRAY_ICON_MAX_DIFF` 就保留抓图。反过来，抓到的若是"闪烁熄灭"那一帧（整块空白，`coverage` ≈ 0），直接丢掉换 exe 图标，否则会把空白当图标存下来。
12. **托盘悬停/点击转发（走图标自己的回调消息，不动光标）**：实现对齐 C# 版 Orbit 的 `TrayClickForwarder`（`Interop/TrayClickForwarder.cs`）。托盘图标注册时会带来 `hWnd` / `uID` / `uCallbackMessage` / `uVersion`，向宿主窗口投这个**回调消息号**并按协议版本编码，就等于"用户把指针放上去/点了键"，完全不用移动系统光标：

    | 版本 | wParam | lParam |
    |---|---|---|
    | 0 / 3 | `uid` | 低 16 位 = 事件 |
    | 4 | 低 16 位 x、高 16 位 y（**屏幕坐标**） | 低 16 位 = 事件、高 16 位 = `uid` |

    **编码必须用图标真实注册的版本**：`Dock::build_host_targets` 直接透传 `Icon.ver`（0/3/4）。曾经一律当成 v4 发，v0 的应用（火绒这类没调 `NIM_SETVERSION` 的）会把 wParam 里的屏幕坐标当成 uid，左右键和悬停全部没反应；微信注册的就是 v4，走的还是同一条路，两不相扰。

    **事件序列照抄 Explorer**（这是更早一次"投消息没反应"的真因：只发 `WM_MOUSEMOVE` 不够）：
    - 进入：`WM_MOUSEHOVER`(0x02A1) → `NIN_POPUPOPEN`(0x0406) → `WM_MOUSEMOVE`；之后每 ~150ms 补一条 `WM_MOUSEMOVE` 维持跟踪。
    - 离开：`WM_MOUSELEAVE`(0x02A3) → `NIN_POPUPCLOSE`(0x0407)。
    - 左键：`AllowSetForegroundWindow(pid)` → `LBUTTONDOWN`（距上次 ≤ `GetDoubleClickTime()` 则 `LBUTTONDBLCLK`）→ `LBUTTONUP` → `NIN_SELECT`(0x0400)（v3+）。
    - 右键：`RBUTTONDOWN` → `RBUTTONUP` → `WM_CONTEXTMENU`（v3+，同样走回调消息，不是发给窗口过程的真 `WM_CONTEXTMENU`）。
    - **悬停坐标必须用图标自己注册的 Placement 矩形中心**（托管模式下由 `trayhost::answer_icon_rect` 回答，指向 Dock 槽位），不能用真实光标：微信会校验指针是否落在自己图标附近，不满足就静默。

    实现见 `src/traymsg.rs`。**参数从哪来**：托管模式下 `hWnd/uID/uCallbackMessage/uVersion/GUID` 全部直接来自 `trayhost::Icon`（从 `WM_COPYDATA` 的 `NOTIFYICONDATA` 解析，见"托盘完整托管"一节），零扫描、零延迟；兜底路径才读 explorer 的 `TRAYDATA` 或扫进程内存找 `NOTIFYICONDATAW`，结果缓存进 `orbit.traycache`。点击链路已验证能让微信从前台切出/切回；悬停弹卡片已实测能被搬到 Dock 上方（微信有未读消息时）。

13. **悬停弹窗重定位**（`src/flyout.rs`，`TRAY_RELOCATE_FLYOUT`）：悬停转发让应用自己弹预览小窗（微信 `Qt51514QWindowToolSaveBits`），但这个窗定位在**旧托盘角**（实测负坐标 (-24,-24)，屏幕左上角外）。所以悬停一开始就起一个后台线程找它可见的 tool window（`WS_EX_TOOLWINDOW` 或类名带 `tool`、尺寸 60×40 ~ 800×900、非最大化、无 `WS_EX_APPWINDOW`；多个候选取面积最小的），找到后用 `SetWindowPos(SWP_NOSIZE|SWP_NOZORDER|SWP_NOACTIVATE|SWP_NOSENDCHANGING)` 摆到 Dock 上方：**水平居中于悬停的托盘图标中心**（`watch()` 传入的 `anchor_x`，先在 Dock 区间内夹取；原来锚 Dock 中心 = 屏幕正中，跟鼠标没关系）、下沿距 Dock 上沿 1px（贴胶囊行上沿），再夹在显示器工作区内。锚点用图标中心而不是实时光标：悬停在 Dock 上会被反复重发，用光标的话鼠标在图标里微动一下卡片就左右跟着滑。**全局 `LATEST` 约束**：多线程可能同时找到同一个卡片 hwnd（微信双开的两个图标按 exe 名互相匹配到对方的卡片），只有最新一次悬停的线程允许 `SetWindowPos`，否则两条线程各按自己的锚点摆，卡片左右乱跳。三个针对微信双开的**关键修正**（实测踩坑）：
    - **归属匹配**：双开时图标宿主 pid 与真正建卡片的进程不同（图标 A 宿主 8504、图标 B 宿主 3608，但两张卡片都由 8504 创建），所以不能只按 pid，要按 exe 名前缀（`weixin`）匹配兄弟进程，否则第二个实例的卡片永远找不到。
    - **不要按"抢前台"排除**：微信的悬停预览卡片自身会抢前台（`GetForegroundWindow()==卡片`），C# 原版那条"抢前台就是真实 UI、撒手"的过滤在实测里恰好把第二个实例的卡片漏掉（用户看到"一个正常、一个弹到左上角"）。改为不因抢前台排除——真正用户点开的是微信主窗口（带 `WS_EX_APPWINDOW` 已排除），卡片被点后会销毁，守位线程自然退出。
    - **generation 按图标 hwnd 分组**（不再全局单值）：两个微信图标各有一条线程，互不取消；同一图标的新悬停才取消它自己的旧线程。弹窗有 2~3s 延迟且闪烁时反复销毁重建，所以搜索窗口 4s、守位期窗口消失后 3.5s 宽限内重新找新 hwnd 接着守。
    摆好后每 140ms 复查、漂移 >2px 就再摆（上限 100 次），最多守 20 秒。系统外壳图标（explorer / ShellExperienceHost）不处理。实测：双开两个微信悬停都把卡片搬到 Dock 上方（(814,751) / (814,695)），全程不动光标。

14. **悬停提示条**：`ORBIT_TIP` 是第四个窗口（分层 + 置顶 + `WS_EX_NOACTIVATE | WS_EX_TRANSPARENT`，点击穿透、不抢焦点），贴在 Dock 上方。托盘图标显示 UIA 的 tooltip 首行（"微信"），任务图标显示窗口标题（拿不到就用程序名）。悬停变化时先重绘 Dock（画出悬停底/椭圆）再定位提示条。
15. **与 Dock 图标去重**：Dock 上已经有的程序，不再重复显示它的托盘图标。判定分两步——托盘元素的 `CurrentProcessId` 在 Win11 上恒为 `explorer.exe`（XAML 树在导出器进程里），所以先按 exe、拿不到再按名字到"有可见窗口的 Dock 项"里做归一化包含匹配。**数量必须一一对应才藏**（`dock 数 == 托盘数`）：微信多开时托盘有 2 个、Dock 只有 1 个（按 exe 去重），这时**两个都保留**——分不清该藏哪一个，藏错了某个账号就彻底没有入口。窗口最小化到托盘（`EnumWindows` 里不再可见）后，该程序不计入"有可见窗口"，托盘图标会自动回到 Dock 右侧。

### 托盘完整托管（trayhost.rs）—— 核心架构

对齐 C# 版 Orbit（ManagedShell）的做法：**不再从 UIA 镜像图标，而是把整个系统托盘接管过来**。这是"隐藏任务栏 + 悬停弹卡片 + 稳定镜像"能同时成立的前提——旧 UIA 方案里任务栏一旦 `SW_HIDE`/autohide/`alpha=1`，XAML 托盘树就消失、`PrintWindow` 变黑，悬停状态也断，根本三者不可兼得。

- **假托盘宿主**：启动时注册 `Shell_TrayWnd` / `TrayNotifyWnd` 两个窗口类并创建假窗口，注册消息 `TaskbarCreated` 广播给 `HWND_BROADCAST`。所有托盘应用收到后会把自己重新 `Shell_NotifyIcon(NIM_ADD)`——消息通过 `WM_COPYDATA` 发到**我们的假窗口**（`FindWindow("Shell_TrayWnd")` 拿到的是 Z 序最前的那个）。
- **图标表**：`WM_COPYDATA(dwData=1)` 的载荷是 `SHELLTRAYDATA`（`+0` magic `0x34753423`、`+4` dwMessage、`+8` packed `NOTIFYICONDATA`，`cbSize=0x3bc`）。按 x64 打包布局（句柄都是 u32）解析出 `hWnd/uID/GUID/uFlags/uCallbackMessage/hIcon/szTip/uVersion/dwState`，维护 `NIM_ADD/MODIFY/DELETE/SETVERSION` 的图标表。这样 GUID 注册的图标（微信 4.x 第二实例 `F915B2DC-…`）、空 tooltip 的图标都能直接拿到——旧方案里这两个是啃不动的硬骨头。`Icon` 的 pid/exe 从图标宿主 `hWnd` 反查。
- **位置查询**：`Shell_NotifyIconGetRect` 由 shell 转成 `WM_COPYDATA(dwData=3)` 发给宿主；`answer_icon_rect()` 用 GUID（优先）或 `hWnd+uID` 匹配图标，返回 Dock 槽位的 packed 坐标（`dm=1` 左上、`dm=2` 右下）。**关键**：要让应用认可这个位置，假窗口必须真的覆盖 Dock 区域——`set_tray_host_size()`（对齐 C# `SetTrayHostSizeData`）在 Dock 每次重画后把假 `Shell_TrayWnd`/`TrayNotifyWnd` 移到 Dock 矩形。实测 `GetRect` 命中返回 Dock 槽位 `(1046,1021)`。
- **置顶守护**：100ms 后台线程把假窗口抬到 `HWND_TOPMOST` 并保持隐藏（`FindWindow` 若返回真任务栏就再抬一次）——这决定了应用把图标注册到我们这里而不是 explorer。
- **退出**：`Host::stop()` 先关消息转发（见下）、重播 `TaskbarCreated` 让应用把图标注册回真任务栏，再销毁假窗口、停守护线程。

**踩过的坑（都已修）**：
- `wndproc` 曾把所有 `msg >= 0x400` 用同步 `SendMessageW` 转发给真任务栏。退出广播 `TaskbarCreated`（注册消息，值 `0xC06A`）时被转发 → explorer 重新注册 → 回发 → **ping-pong 无限递归栈溢出**，进程在 `restore_all` 中途崩溃、日志断在 `restore hwnd`。修复三重防护：注册消息（`>= 0xC000`）不转发、`stop()` 期间 `FORWARD_ENABLED=false`、转发用 CAS 重入保护 + `SendMessageTimeoutW`。
- `win_event_cb`（任务栏复显钩子）里调 `ABM_SETSTATE` 也会触发同样的递归，已移除，钩子只做 `SWP_HIDEWINDOW`，并加 CAS 重入保护。

### 托盘镜像（tray.rs）
镜像数据层**优先消费 `trayhost` 的图标表**（`attach_host()` → `poll_hosted()`）：图标像素直接用 `capture_icon_rgba(HICON)` 画托管拿到的 `hIcon`，比从任务栏位图抠 16px 清晰且稳定；tooltip、exe、pid 都由托管表提供。闪烁同步改为直接观察 `NIM_MODIFY(NIF_ICON, hIcon=0)` 的"灭帧"（`Icon.dim`），不再 `PrintWindow` 采样墨量。

`config::TRAY_TAKEOVER=false` 或未托管成功时退回旧的 **UIA + PrintWindow** 路径：

- **枚举**：`IUIAutomation::ElementFromHandle(Shell_TrayWnd)` → `FindAll(Descendants, And(ControlType=Button, AutomationId="NotifyItemIcon"))`。
- **图标像素**：`PrintWindow(Shell_TrayWnd, PW_RENDERFULLCONTENT)` 画整条任务栏，按 `Image` 子元素矩形裁出，再按 padding 环取底色反解 alpha；放大走 Catmull-Rom。
- **防抖/避让**：连续 3 次扫空才认账（XAML 重建期会短暂返回 0 个元素）；`WindowFromPoint`+`GetAncestor` 判定鼠标压在任务栏图标上时跳过抓图。
- **点击**：左键 `LegacyIAccessible.DoDefaultAction`；右键因 UIA 无动作，只能临时移光标发 `SendInput` 右键再放回——**仅兜底路径如此；托管路径全程不动光标**。
- **多开**：按元素枚举，绝不按名字/exe 合并，`current_process_id` 在 Win11 恒为 explorer，所以按 exe + 可见窗口名匹配去重，且"数量一一对应才藏"（微信双开托盘 2 个、Dock 1 个时两个都保留）。
- **与 Dock 的去重方向**（`dock.rs::hide_dock_duplicates`）：同一个进程的图标同时出现在托盘和 Dock 应用位时，**隐藏 Dock 的"任务栏图标"、托盘图标保留**（设置页单独隐藏的托盘图标除外，此时 Dock 图标照常显示）。explorer 托管的图标 exe 拿不到时按 tooltip 归一化名字兜底匹配。

### 原生任务栏隐藏（默认开启）
托盘已托管，不再依赖 UIA/PrintWindow，因此可以直接**真隐藏**任务栏（对齐 C# `ExplorerHelper`）：

- **默认接管**：不带参数启动就隐藏（`--show-taskbar` 关掉）。`SHAppBarMessage(ABM_SETSTATE, ABS_AUTOHIDE)` + `SetWindowPos(HWND_BOTTOM, SWP_HIDEWINDOW)`，`SPI_SETWORKAREA` 放开工作区，`IsWindowVisible=false`、不占工作区。
- **排除假窗口**：`EnumWindows` 枚举 `Shell_TrayWnd` 时用 `trayhost::is_own_hwnd()` 排除我们自己创建的假宿主，否则会把自己藏掉；副屏 `Shell_SecondaryTrayWnd` 同样处理。
- **复显守护**：`SetWinEventHook(EVENT_OBJECT_SHOW)`（带 CAS 重入保护）+ 100ms watchdog `ensure_hidden()`，explorer 重启/换主题把任务栏显示回来时立刻按回去。
- **退出恢复**：隐藏前记录每个任务栏的原始矩形、ABM 状态、工作区；`restore_all()` 恢复后**带验证地补恢复**——`stop()` 广播的 `TaskbarCreated` 由 explorer 异步处理，会在恢复之后把任务栏重新设为 autohide 并滑出屏幕（实测 ~150ms 内）。所以每 150ms 检查一次"无 autohide + 可见 + 位置归位"，被覆盖就再恢复，直到稳定（最多 8 次）。实测退出后 `state=0x0 visible=true rect=(0,1032,1920,1080)` 完全归位。
- **强杀兜底**：进程被强杀不走恢复路径，用 `--restore-taskbar`（`hide::force_restore()`）手动救。

## 已知限制
- **托盘托管依赖 `TaskbarCreated` 重注册**：接管瞬间会广播该消息，少数不重注册托盘图标的程序可能短暂丢失图标（重启应用即可）。假窗口生命周期与 Orbit 绑定，退出重播 `TaskbarCreated` 让图标回真任务栏。
- **兜底 UIA 路径的溢出区限制**：`TRAY_TAKEOVER=false` 时，溢出面板里的图标只有展开才在 UIA 树中。托管路径不受此限——应用重新注册时逐个 `NIM_ADD`，包括原本在溢出区的。
- **托盘图标清晰度**：托管模式直接用 exe/应用提供的 `HICON`（经 `appicon` 优先取 256px），比旧方案从任务栏抠 16px 放大清晰得多；但个别应用只注册小 `hIcon` 时仍会略软。
- **右键菜单位置**：由应用在自己图标的 Placement（已上报为 Dock 槽位）处弹出，一般贴着 Dock 图标；个别应用忽略 Placement 时位置可能偏离。
- 邮件正文只取前 16KB（阅读窗展示用，超出部分截断）。
- 邮件已读写回是"尽力而为"：QQ 邮箱偶尔会因连接被复用超时导致某次 STORE 失败（本地已读仍保留，再点一次即可补上）。
- 剪贴板只记文字；复制文件/图片不记录（有意为之）。
- 歌词只显示原文（LRC 的 `tlyric` 翻译没用上）、只显示一行，没有逐字卡拉 OK；搜歌靠"歌名 + 歌手"打分，同名翻唱可能匹配到别的版本。
- 专辑封面只在换曲时取一次；有些播放器（尤其是浏览器里的网页播放器）根本不给 `Thumbnail`，这时显示占位音符而不是封面。封面被缩到 32px 后旋转，细节本来就看不清，够用即可。
- 播放器时间线残缺时（只在换曲时写 Position），播放头依赖 `LastUpdatedTime`；若启动时正好暂停在那一曲，首句可能偏。
- Dock 未区分"是否在运行"，无 hover 缩略图、虚拟桌面过滤、多屏。
- 隐藏任务栏走 autohide + `SWP_HIDEWINDOW`，退出恢复带 8 次带验证补恢复；进程被强杀时任务栏停留在隐藏状态，需 `--restore-taskbar` 手动救。

## 路线图
1. **托盘溢出区**：需要（临时）展开溢出面板采集隐藏图标，或读 `HKCU\Control Panel\NotifyIconSettings` 把 `IsPromoted` 打开。
2. **设置页面继续补内容**：已有 `settingsui.rs`（时钟格式串编辑 + 实时预览、胶囊透明度/形状、Dock 锚位、模块芯片拖动排序与显隐、天气数据源与 Key、邮箱账户、歌词位置/字号、天气字号倍率、托盘与返回桌面开关、逐个托盘图标显隐），还没做的是主题/配色、天气城市手动指定、Dock 图标个数上限的调整入口。
3. 邮件：按需拉正文全文、支持多账号。
4. 歌词：翻译/罗马音、逐字高亮、无歌词时手动搜索换一首。
5. 面板：键盘导航、滚动（超过一屏的历史）、邮件全文预览。
6. 看门狗进程、Dock 缩略图。
