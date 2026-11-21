# 文件索引（AI 开发入口）

> 先读本表定位文件，再读具体代码，不要全量扫描仓库。
> **铁律：新增/删除/重命名文件，或文件职责变化时，必须同步更新本表。**

## 源码

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 程序入口：`#![windows_subsystem = "windows"]`（不弹控制台）、初始化日志、解析 `--show-taskbar` / `--restore-taskbar`，调用 `app::run`（默认接管任务栏） |
| `src/appmsg.rs` | 跨线程「设置已应用」即时通知：`WM_SETTINGS_APPLIED`（WM_APP+0x72）；胶囊 / Dock / 天气卡片窗口 init 时登记各自 HWND（raw isize，跨线程安全），设置页 `commit/edit` 后 `notify_applied()` 给三者各投一条消息，收到就立即按新版本重绘，不必等 500ms 轮询。定时器轮询保留作手改文件兜底 |
| `src/notify.rs` | 内嵌提示音（mp3 编进 exe，MCI 播放）：**单条播放线程 + 队列**，`play_banner()`（横幅弹出，每条横幅排一声，不再做时间合并）与 `play_wechat()`（托盘闪烁，按图标 key 200ms 去重）都入队，worker 每声 `seek to start` 后播完（轮询 `status mode`）才放下一声——多声不互相截断，听感上每条都实打实响一次；队列上限 8 条防堆积 |
| `src/weathercard.rs` | **桌面右上角当地天气卡片**：独立分层窗口（`WS_EX_LAYERED\|TOOLWINDOW\|NOACTIVATE`，**不置顶**——每次显示都 `HWND_BOTTOM` 压到桌面层，普通窗口都能盖住它），鼠标 `HTTRANSPARENT` 穿透、不抢焦点；透明背景（不画底板，逐像素 alpha），只画「当地天气 / 图标+粗体大温度 / 天气·城市 / 湿度风力 / 来源」。**配色自适应壁纸**：沿卡片边缘取屏幕像素（`GetPixel`，配 `WindowFromPoint` 判桌面可见）估平均亮度，深壁纸用浅色字、浅壁纸用深色字，再配一层反差色淡描影；每 2s 复检，被窗口挡住时不采样不乱切。天气已从胶囊移出——数据由主 App `weather.tick()` 拿到快照后 `update()` 推来（全局 `Mutex<Option<View>>`），监听 `WM_SETTINGS_APPLIED` 即时换主题/字体。位置：工作区右上角留 32 逻辑点边距（不贴边）。显隐复用 `module_mode("weather")`：off=隐藏，其余=右上角显示；**另有 500ms 显隐自检定时器**（`recheck_vis`）——Win+D 显示桌面会把卡片一起最小化，检测到被收起且开关未隐藏时自动恢复并压回桌面层（天气常驻桌面、不被 Win+D 收走） |
| `src/app.rs` | 启动流程：GDI+ / COM 初始化、注册各窗口类（胶囊 / Dock / 提示条 / 邮件阅读窗 / 横幅 / 天气卡片）、创建胶囊与 Dock、消息循环、退出清理（恢复任务栏） |
| `src/window.rs` | 顶部胶囊窗口：置顶分层窗口、widget 布局与命中（**天气已移出胶囊**，改右上角独立卡片，见 weathercard.rs）、悬停、时钟定时器（带秒的格式下每秒重绘）、展开面板的开关/点击分发、音乐条的显隐/角度驱动、**按配置把 widget 分到胶囊 / 左右模块条**、**胶囊右侧的托盘闪烁图标**（`T_FLASH` 70ms 比 `dock::flash_version()`，悬停/点击转发给真实托盘）；水平居中只按胶囊自身宽度（分离出去的模块条不参与）；`primary_monitor()` 也在这里 |
| `src/clock.rs` | **胶囊里的时钟模块**：把日期与时间**合并成一段**文本（20:15 11月20日 周五），排列（`Order`：时间在前/日期在前/只其一）、时间格式（`TimeStyle`：24h / 24h 带秒 / 12h 上午下午 / 12h 带秒 / 中文「20时15分」）、日期格式（`DateStyle`：月日周 / 月日 / 月日短 / 周X / 星期X / ymd / ymd 斜杠 / 中文年月日）各有编解码与 `parse`（settings.txt 用）；`text()` 拼给胶囊、`sample_*()` / `sample_order()` 给设置页单选块做实时预览（`all()` 给出候选顺序）。纯格式化，不绘制 |
| `src/dock.rs` | 底部 Dock 窗口：钉选 `.lnk` + 运行中应用（按 exe 去重）、大图标提取与缩放、点击启动/切换窗口；**右侧托盘区**（持有 `TrayMirror`、2s 轮询、左右键/悬停按图标**真实协议版本**转发 v0/v3/v4、悬停提示条、与 Dock 图标去重）；**最右端返回桌面细长条**（悬停变亮 + 提示条，点击 `SendInput` 发一次 **Win+D**——原 `IShellDispatch4::ToggleDesktop` 在任务栏被接管后实测不生效）；托盘显隐 = 与 Dock 图标去重 + 按 `settings::hide_tray` 名单单独隐藏；**把「正在闪烁的图标」发布成胶囊消费的快照**（`flash_icons()` / `flash_version()`，见 `FlashIcon`）；右键空白处打开设置页 |
| `src/panel.rs` | 展开面板窗口（天气卡片 / 剪贴板 / 邮件）：独立分层窗口、命中表 → 动作号、点击回传 `WM_ROW_CLICK`，自身不做业务判断 |
| `src/musicbar.rs` | 音乐条窗口：贴在胶囊左/右/下方的小窗 = 旋转的圆形专辑封面（中心播放/暂停按钮）+ 当前歌词；没有播放时整条隐藏；只有封面圆接鼠标（左键播放/暂停、右键下一首），其余穿透 |
| `src/widgetbar.rs` | **分离模块条**窗口：每侧最多 `WIDGET_BAR_MAX_SLOT`(3) 条独立分层小窗，按 (侧, 编号) 一一对应、懒创建。`window.rs` 把同侧同编号的 widget 合成一组传进来（`sync(Frame)` 一帧统一定位/显隐，从胶囊往外按编号排，编号 1 贴得最近），条与胶囊同高同底色；整窗默认穿透，只有 widget 矩形接鼠标，点击 `PostMessage(胶囊, WM_WIDGET_CLICK, 类型号)` 由 `App::on_widget` 分发 |
| `src/render.rs` | **仅剩非绘制能力**（UI 绘制已全迁 egui）：GDI+ 生命周期（startup/shutdown）、`present()` 上屏（premultiplied BGRA → UpdateLayeredWindow）、程序/托盘图标捕获 `capture_icon_rgba`、高质量缩放 `resize_rgba`、封面 `decode_cover`/`rotate_cover`、文本测量 `measure_text`（供面板尺寸预算）、`premultiplied_rgba_to_bgra`；数据结构 `WidgetItem`/`FlashView`/`PillShape`/`PanelRow`/`WeatherView` 等 |
| `src/egui_soft.rs` | **公共 egui 软件后端**：持有 `egui::Context` + 纹理表；`rasterize()` 把 epaint 网格光栅化成预乘 RGBA（`fill_tri` 重心插值 + 双线性纹理采样 + 预乘 over 合成）；`soft_image()` 上传 straight RGBA 像素块成 egui 纹理（内容 hash 复用）；`install_fonts`/`build_font_definitions` 注入字体（默认 MiSans **Semibold** 一款排最前，磁盘加载不打进 exe，缺失回落系统字体，`orbit-bold` 粗体族同指 Semibold，系统字体兜底；`font=system` 回落纯系统字体+雅黑 Bold，切换后 `set_scale` 重建图集+度量通道）；字体字节按「路径+大小+mtime」全局缓存为 `Arc<FontData>`，8 个后端共享；`clock_font()` 时钟/日期统一走 `bold_font()` 粗体族；`argb`/`divider` 工具。所有分层窗口共用这套模式（egui 逻辑点布局 → 本后端 → `present()`） |
| `src/pillui.rs` | 胶囊的 egui 绘制（window.rs 持 `PillUi`）：时钟（时间+日期走 `bold_font()` 真粗体）+ widget 区 + 闪烁图标格；预量宽用同一粗体 FontId 的 egui 字体度量，命中矩形按逻辑点收集后乘 scale；`PillEvent`（widget/闪烁图标点击与悬停进出）回 wndproc 按旧语义分发；变化检测 hash 让纯鼠标路过帧跳过上屏 |
| `src/dockview.rs` | Dock + 悬停提示条的 egui 绘制：`paint()` 画底板/应用图标/分隔线/托盘图标/返回桌面细条（支持竖排四锚位），`paint_tooltip()` 画提示条；`dock_size()` 逻辑尺寸预算；图标字节 `soft_image()` 上传后画；命中矩形物理像素返回给 dock.rs 的 wndproc |
| `src/widgetbarview.rs` | 分离模块条的 egui 绘制：与胶囊 widget 区同构（圆角底 + 图标字 + 文本 + 悬停底），`bar_width()` 逻辑宽度预算；每窗口在 widgetbar.rs 里 thread_local 一份 `SoftBackend` |
| `src/musicbarview.rs` | 音乐条的 egui 绘制：旋转封面（`render::rotate_cover` 后上传纹理）+ 占位圆/音符 + 中心播放/暂停按钮（convex_polygon 三角 / 两根竖条）+ 歌词；`cover_box`/`fixed_w`/`text_w`/`ellipsize` 逻辑尺寸工具；musicbar.rs 里 thread_local 一份 `SoftBackend` |
| `src/panelview.rs` | 展开面板的 egui 绘制：`paint_list()` 通用列表（标题/副标题/标签页/行/固定按钮，命中区动作号 `0..999`/`PIN_BASE+`/`TAB_BASE+` 不变）、`paint_weather()` 天气宽卡片（左栏实况 + 右栏四天预报 + 刷新按钮，字号倍率 k）；面板尺寸仍复用 `render::panel_size`/`weather_panel_size`（纯字体测量） |
| `src/weather.rs` | 天气数据：高德（实况 + 四天预报，失败回退 Open-Meteo）与出口 IP 定位，30 分钟刷新、失败指数退避；`describe()/glyph()` 码位映射 |
| `src/lyric.rs` | 歌词数据：按曲目搜网易云 → 取 LRC → 解析时间轴 → 按播放进度取当前行；换歌才请求，结果（含"无歌词"）内存缓存 |
| `src/mail.rs` | 邮箱数据：IMAP over TLS（最小 IMAP4rev1 子集）拉最近 N 封、MIME 解码、验证码抽取；已读双向同步（读服务端 `\Seen`，点开时后台 `UID STORE +FLAGS (\Seen)` 写回）；`test_account()` 供设置页「测试连通」（建连 + LOGIN 实测） |
| `src/mailview.rs` | 邮件阅读窗：点邮件标题弹出的普通 popup 窗（分层面板带不了子控件），只读多行 EDIT 呈正文（可选中/Ctrl+C/滚动），深色主题；「删除邮件」经 `WM_COPYDATA`(dwData=0x4D41494C) 把 UID 发回胶囊 → `Mail::delete`（本地移除 + 后台 `\Deleted`+EXPUNGE 真删） |
| `src/music.rs` | 音乐数据：SMTC 读当前曲目 + 播放进度（锚点 + 本地推进）+ 专辑封面字节，播放/暂停/切歌；采样在独立 MTA 线程 |
| `src/clipboard.rs` | 剪贴板历史：`AddClipboardFormatListener` 监听线程、只收文字（文件/图片过滤）、去重、固定置顶、敏感内容过滤、落盘 |
| `src/settings.rs` | 可调设置：`<exe>/data/settings.txt`（key=value），运行时查 mtime 热更新；键：`clock_order` / `time_format` / `date_format`（时钟模块，见 `clock.rs`）/ `hide_tray`（逐个托盘图标隐藏，配合 `tray_hidden()` / `set_tray_hidden()` / `clear_hidden_tray()`）/ `weather_text_scale` / `lyric_position` / `lyric_size` / `weather_pos` / `mail_pos` / `clipboard_pos`（`pill` / `left[2|3]` / `right[2|3]`，`Place{pos,slot}`：同侧同编号合并成一条、编号不同各自一条）/ `show_tray`（仅手改文件，设置页不再提供总开关）/ `show_desktop_btn`（设置页在「托盘图标」列表第一格开关）。`set_*` 供设置页调用（改内存 + 就地改文件 + `VERSION += 1`），配置页写盘只替换受管键、保留注释与用户自加的键。**编辑会话**：`begin_edit()` 后所有 `set_*` 只改内存不落盘不 bump，点「应用」`commit_edit()` 统一 `save()+bump()`，「还原」/关窗 `discard_edit()` 从文件 `load()` 恢复；提交/还原后经 `appmsg::notify_applied()` 通知胶囊/Dock/天气卡片立即重绘 |
| `src/settingsui.rs` | **设置页面（egui 版）**：`eframe`（glow 后端，含 clipboard feature）跑在独立后台线程（`event_loop_builder` 开 `with_any_thread`，首次 show 懒启动），无边框小窗（不置顶，`with_taskbar(false)` 不占任务栏），主线程经 `QUEUE` + `Context::request_repaint` 收发显示/隐藏命令，`App::logic` 消费命令并拦截 Alt+F4（当作收起，eframe 常驻）。`NativeOptions::centered=true` 让窗口在建窗前就居中（默认 false 会先出现在左上角再跳到中间，看起来"闪一下"）。**分组导航布局**：左栏 6 个分组按钮，右栏画所选分组详情（组内滚动；ScrollArea 内容须显式 top_down 防布局继承）。分组：顶部胶囊、Dock 位置、音乐条、天气、邮箱账户、模块与托盘。改值直接调 `settings::set_*`，胶囊与 Dock 靠 `settings::version()` 重绘；编辑框本地缓冲、失焦才写盘；固定 940×560、打开时按 DPI 换算主屏居中；标题/分组/行标签用 `egui_soft::bold()` 走真实粗体族；模块区「位置下拉 + ▲▼ 排序」；**天气行只有 显示/隐藏 开关**；**「手动拿 UIA」下逐实例显示微信多开的拿取状态**。顶栏「应用/还原」按钮管理编辑会话。右键 Dock 空白、右键胶囊、`Ctrl+Alt+,` 打开 |
| `assets/fonts/MiSans-Semibold.ttf` | 内置唯一界面字体（Semibold，约 7.7 MB）：`egui_soft.rs` 用 `include_bytes!` 编进 exe，`font=misans`（默认）时排在所有 egui 字体族最前，`orbit-bold` 粗体族也指向它；无外部文件依赖 |
| `assets/fonts/MiSans-Semibold.ttf` | 界面主字体（中英文统一字形）：**不编进 exe**，运行时从磁盘加载（发布压缩包放 `<exe>/data/MiSans-Semibold.ttf`；开发期走 `assets/fonts/`），排在所有字体族最前；文件缺失回落系统字体 |
| `src/store.rs` | 唯一持久化入口：所有数据文件都落在 `<exe 目录>/data/`，不碰注册表和安装目录以外的地方 |
| `src/net.rs` | 极简 HTTP(S) GET（WinHTTP，走系统代理） |
| `src/json.rs` | 极小 JSON 解析器（object/array/string/number） |
| `src/trayhost.rs` | **完整托盘托管（默认开启，`config::TRAY_TAKEOVER`）**：注册假 `Shell_TrayWnd`/`TrayNotifyWnd` 窗口类 + 广播 `TaskbarCreated`，让应用把托盘图标重新注册到我们这里；`WM_COPYDATA(dwData=1)` 解析 packed `NOTIFYICONDATA` 建图标表（hWnd/uID/GUID/回调消息/版本/hIcon/tooltip 全拿到，不依赖 UIA）；`dwData=3` 回答 `Shell_NotifyIconGetRect` 位置查询；`set_tray_host_size()` 把假窗口移到 Dock 矩形（对齐 C# `SetTrayHostSizeData`）；100ms 守护把假窗口置 TOPMOST 并保持隐藏；消息转发带注册消息过滤 + 重入保护（防退出广播 TaskbarCreated 触发栈溢出）。移植自 C# Orbit / ManagedShell |
| `src/tray.rs` | 托盘镜像数据层：**优先消费 `trayhost` 的托管图标表**（`attach_host`/`poll_hosted`，直接拿 `NIM_MODIFY(hIcon=0)` 的灭帧做闪烁同步，不再 PrintWindow 采样像素）；未托管时退回 UIA + PrintWindow（`NotifyItemIcon` 枚举、exe 大图标 + 相似度校验）。图标来源统一走 `appicon`；`TrayItem::key` / `icon_key()` 给出跨会话稳定的图标键（GUID → exe+uID → 名字），设置页靠它记住哪个图标被隐藏；**稳定排序 `stabilize()`**：按首次出现顺序固定图标位置（重注册 DELETE+ADD 不挪位），新图标插到同 exe 最后一个实例之后（多开微信默认挨着、先开的在前、新创建的在后），无同类追加末尾；只提供数据与状态，不绘制、不持有窗口 |
| `src/traymsg.rs` | 托盘**回调消息转发**：托管模式下目标（`hWnd/uID/uCallbackMessage/uVersion/GUID`）直接来自 `trayhost::Icon`；旧路径从 explorer 的 `TRAYDATA`（`TB_GETBUTTON`+`ReadProcessMemory`）或进程内存 `NOTIFYICONDATAW` 挖。按 v0/3/4 编码投 `WM_MOUSEHOVER`/`NIN_POPUPOPEN`/`WM_MOUSEMOVE`/`WM_MOUSELEAVE`/`NIN_POPUPCLOSE`/左键/右键（含 `AllowSetForegroundWindow` + `NIN_SELECT`）|
| `src/flyout.rs` | **悬停弹窗重定位**：应用悬停时自弹的预览小窗（微信 tool window）会落在旧托盘角（实测左上角 (-24,-24)），后台线程按 exe 名找兄弟进程（双开时卡片进程 ≠ 图标宿主）、`SetWindowPos` 摆到 Dock 上方并守位。**不按"抢前台"排除**（微信卡片自身抢前台）、generation 按图标 hwnd 分组（两实例互不取消）、窗口销毁重建后重新锁定。移植自 C# `HoverFlyoutRelocator` 并针对微信双开加固 |
| `src/appicon.rs` | 托盘图标的高分辨率来源：读 `HKCU\Control Panel\NotifyIconSettings\*` 的 `InitialTooltip`/`ExecutablePath` 建"名字 → exe"表（进程名兜底），再从 exe 取 256px 图标；表与图标都带缓存 |
| `src/wechatmsg.rs` | **微信消息源（只读 UIA，参照 Atrium `wechat_source.cpp`）**：MTA 线程 `CUIAutomation` + `FindAllBuildCache(ClassName=mmui::ChatSessionCell)` 读会话行，解析会话名/`[N条]`/摘要/免打扰；未读数上升才弹横幅（首读建基线、免打扰不发）。**前台跳过按进程隔离**：只跳过处于前台的那一个微信进程（按键期 UIA 读它会卡），多开的另一个实例照常读、照常弹横幅。多开：遍历所有微信进程所有窗口，baseline 键 `pid\|AutomationId` 隔开；`held` 缓存也带 pid，前台进程的元素原样保留、等它退到后台再续读。关到托盘后用带 pid 的 `held` 元素 `BuildUpdatedCache` 续读。`Reader::wake()` 供托盘闪帧即时唤醒；`request_grab()` 供设置页「手动拿 UIA」全量抓一次并提示结果；**`GRABBED` 状态表 + `instances()`**：逐实例记录 UIA 拿取状态（读到会话行或仍有 held 缓存即拿到），`instances()` 按进程创建时间排序（先开的在前，2s 缓存），设置页据此显示多开哪个拿了哪个没拿 |
| `src/guard.rs` | **系统级功能**：开机自启（`HKCU\...\Run` 写 `Orbit` 值，`apply_autostart`/`sync_autostart`）、退出守护（主进程隐藏任务栏后拉起 `orbit.exe --guardian=<pid>` 子进程，父进程正常退出留 `data/orbit.exit` 标记；强杀则子进程 `force_restore` + 重启 explorer 救回任务栏）、电源动作（关机/重启 `ExitWindowsEx`、锁屏 `LockWorkStation`）、单实例互斥（`acquire_single_instance`） |
| `src/startmenu.rs` | **开始菜单（接管系统 Win 键）**：按 Win 弹出、再按收起、点外收起（WH_MOUSE_LL 钩子）。上栏应用快捷方式网格（设置页可增删，默认 cmd），下栏关机/重启/锁屏小菜单 + 设置 + 系统设置（ms-settings:）；窗口形态与 banner.rs 同款分层小窗 + egui 软件光栅化 |
| `src/banner.rs` | **通用右下角横幅窗口**：分层置顶不抢焦点小窗（egui 软件后端，跟随主题），多条堆叠递增（新条在下、6 秒、悬停暂停、点哪张收哪张、最多 5 条），位置在 Dock 上方右下角。`show(BannerNotice)` 跨线程可调用（`WM_BANNER` + Box 投递），`BannerNotice::info()` 通用提示 / `::wechat()` 微信卡片；每次弹出播提示音（可静音）。`on_flash()` 去重唤醒读取；`request_grab()` 触发手动抓取；500ms 定时器按 `settings::wechat_cards()` 启停 `Reader` |
| `src/hide.rs` | 原生任务栏**真隐藏**/恢复（托盘已托管，不依赖 UIA）：`SHAppBarMessage(ABM_SETSTATE, ABS_AUTOHIDE)` + `SetWindowPos(HWND_BOTTOM, SWP_HIDEWINDOW)`，并放开工作区；排除 `trayhost` 的假窗口；WinEventHook（`EVENT_OBJECT_SHOW`，带重入保护）监视复显，100ms watchdog 补隐藏；隐藏前记录原始矩形/ABM 状态/工作区，退出时 `restore_all()` 带验证补恢复（抗 explorer 异步 `TaskbarCreated` 覆盖），最多 8 次；`force_restore` 供 `--restore-taskbar` 强杀后恢复 |
| `src/trace.rs` | 日志：`<exe>/orbit.log`（启动时清空），`tlog!` 常记、`vlog!` 仅 `ORBIT_TRACE=1` 时记（无控制台也能排查） |
| `src/config.rs` | 全部样式/行为常量（尺寸、颜色、透明度、widget 开关、Dock 参数、天气参数）；`TRAY_TAKEOVER=true` 开托盘完整托管、`SHOW_TRAY`/`RELOCATE_FLYOUT` 等开关 |

## 开发工具

| 文件 | 职责 |
|---|---|
| `tools/traytest/` | 托盘测试程序：注册 N 个托盘图标（可多实例、同名校验），点击打印回调，用来验证托盘镜像；**不要用微信测**（多开会混淆） |
| `tools/trayhostprobe/` | 机制验证探针：注册假 `Shell_TrayWnd` + 广播 `TaskbarCreated`，打印收到的 `WM_COPYDATA` 原始字节（确认系统会把托盘注册发给假窗口）；`restore` 子命令重播 TaskbarCreated |
| `tools/trayhostcheck/` | harness：用 `#[path]` 直接引入产品模块 `src/trayhost.rs`，不启动完整 Orbit 就能验证图标表捕获（GUID 图标、空名字图标、`NIM_ADD/MODIFY/DELETE`） |
| `tools/hidecheck/` | 任务栏隐藏 A/B 工具：`hide`（autohide+SWP_HIDEWINDOW）/ `show`（还原）/ `back`（移回原位）/ `unlayer`（清分层残留），打印 state/visible/rect/workarea |
| `tools/flyouttest/` | 悬停弹窗重定位的离线验证：`#[path]` 引入 `flyout.rs`，派生子进程造一个 `WS_POPUP\|WS_EX_TOOLWINDOW` 假弹窗（初始摆在旧托盘角 1500,900），调 `flyout::watch(..., anchor_x)` 后打印实际位置 vs 期望（鼠标 x 居中、下沿贴胶囊上沿）。`flyouttest [anchor_x]`，实测 1043→(883,595)、1900→(982,595) 夹取正确；不依赖微信/不碰任务栏 |

## 文档

| 文件 | 职责 |
|---|---|
| `docs/FILEMAP.md` | 本文件，文件索引 |
| `docs/ARCHITECTURE.md` | 架构总览、widget/托盘/Dock 设计、已知限制、路线图 |
| `docs/HANDOVER.md` | 交接文档：构建/运行命令、当前状态、过期代码清理记录、开发约定 |

## 运行

```bash
cargo run --release                      # 胶囊 + Dock，默认接管（隐藏）原生任务栏，退出自动恢复
cargo run --release -- --show-taskbar    # 保留原生任务栏（调试用，托盘镜像仍然工作）
cargo run --release -- --restore-taskbar # 进程被强杀、任务栏未归位时手动恢复
```

托盘托管机制：假 `Shell_TrayWnd` + `TaskbarCreated` 完整接管系统托盘，图标的 hWnd/uID/GUID/
回调消息/版本/hIcon 全部从 `WM_COPYDATA` 的 `NOTIFYICONDATA` 直接拿到（不依赖 UIA），
所以任务栏能被真正隐藏（`SWP_HIDEWINDOW`）+ 悬停弹卡片 + 稳定镜像三者同时成立。
`ORBIT_TRACE=1` 会记录 `[trayhost]`（图标注册/GetRect 命中）、`[hide]`（隐藏/恢复）、
`[dock]`（悬停转发/去重）细节。

日志：`<exe 目录>/orbit.log`（每次启动清空）。托盘调试用 `ORBIT_TRACE=1 cargo run --release`，
会把每次托盘扫描（名字 + 所属 exe）、去重结果、Dock 槽位矩形、提示条位置写进该文件。

退出热键：`Ctrl+Alt+O`（两个窗口都不抢焦点，必须留这条通道）。
