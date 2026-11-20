# 文件索引（AI 开发入口）

> 先读本表定位文件，再读具体代码，不要全量扫描仓库。
> **铁律：新增/删除/重命名文件，或文件职责变化时，必须同步更新本表。**

## 源码

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 程序入口：`#![windows_subsystem = "windows"]`（不弹控制台）、初始化日志、解析 `--show-taskbar` / `--restore-taskbar`，调用 `app::run`（默认接管任务栏） |
| `src/app.rs` | 启动流程：GDI+ / COM 初始化、注册四个窗口类（胶囊 / Dock / 提示条 / 邮件阅读窗）、创建胶囊与 Dock、消息循环、退出清理（恢复任务栏） |
| `src/window.rs` | 顶部胶囊窗口：置顶分层窗口、widget 布局与命中、悬停、时钟定时器、展开面板的开关/点击分发、音乐条的显隐/角度驱动；`primary_monitor()` 也在这里 |
| `src/dock.rs` | 底部 Dock 窗口：钉选 `.lnk` + 运行中应用（按 exe 去重）、大图标提取与缩放、点击启动/切换窗口；**右侧托盘区**（持有 `TrayMirror`、2s 轮询、左右键转发、悬停提示条、与 Dock 图标去重） |
| `src/panel.rs` | 展开面板窗口（天气卡片 / 剪贴板 / 邮件）：独立分层窗口、命中表 → 动作号、点击回传 `WM_ROW_CLICK`，自身不做业务判断 |
| `src/musicbar.rs` | 音乐条窗口：贴在胶囊左/右/下方的小窗 = 旋转的圆形专辑封面（中心播放/暂停按钮）+ 当前歌词；没有播放时整条隐藏；只有封面圆接鼠标（左键播放/暂停、右键下一首），其余穿透 |
| `src/render.rs` | 全部绘制：胶囊/ Dock 圆角底板、文本（三种字体族）、widget、图标合成、面板与天气卡片、音乐条（封面解码/旋转/按钮/歌词）、`present()` 上屏、图标捕获、高质量缩放、BGRA 转换 |
| `src/weather.rs` | 天气数据：高德（实况 + 四天预报，失败回退 Open-Meteo）与出口 IP 定位，30 分钟刷新、失败指数退避；`describe()/glyph()` 码位映射 |
| `src/lyric.rs` | 歌词数据：按曲目搜网易云 → 取 LRC → 解析时间轴 → 按播放进度取当前行；换歌才请求，结果（含"无歌词"）内存缓存 |
| `src/mail.rs` | 邮箱数据：IMAP over TLS（最小 IMAP4rev1 子集）拉最近 N 封、MIME 解码、验证码抽取；已读双向同步（读服务端 `\Seen`，点开时后台 `UID STORE +FLAGS (\Seen)` 写回） |
| `src/mailview.rs` | 邮件阅读窗：点邮件标题弹出的普通 popup 窗（分层面板带不了子控件），只读多行 EDIT 呈正文（可选中/Ctrl+C/滚动），深色主题；「删除邮件」经 `WM_COPYDATA`(dwData=0x4D41494C) 把 UID 发回胶囊 → `Mail::delete`（本地移除 + 后台 `\Deleted`+EXPUNGE 真删） |
| `src/music.rs` | 音乐数据：SMTC 读当前曲目 + 播放进度（锚点 + 本地推进）+ 专辑封面字节，播放/暂停/切歌；采样在独立 MTA 线程 |
| `src/clipboard.rs` | 剪贴板历史：`AddClipboardFormatListener` 监听线程、只收文字（文件/图片过滤）、去重、固定置顶、敏感内容过滤、落盘 |
| `src/settings.rs` | 可调设置：`<exe>/data/settings.txt`（key=value），运行时查 mtime 热更新；`weather_text_scale` / `lyric_position` / `lyric_size` |
| `src/store.rs` | 唯一持久化入口：所有数据文件都落在 `<exe 目录>/data/`，不碰注册表和安装目录以外的地方 |
| `src/net.rs` | 极简 HTTP(S) GET（WinHTTP，走系统代理） |
| `src/json.rs` | 极小 JSON 解析器（object/array/string/number） |
| `src/trayhost.rs` | **完整托盘托管（默认开启，`config::TRAY_TAKEOVER`）**：注册假 `Shell_TrayWnd`/`TrayNotifyWnd` 窗口类 + 广播 `TaskbarCreated`，让应用把托盘图标重新注册到我们这里；`WM_COPYDATA(dwData=1)` 解析 packed `NOTIFYICONDATA` 建图标表（hWnd/uID/GUID/回调消息/版本/hIcon/tooltip 全拿到，不依赖 UIA）；`dwData=3` 回答 `Shell_NotifyIconGetRect` 位置查询；`set_tray_host_size()` 把假窗口移到 Dock 矩形（对齐 C# `SetTrayHostSizeData`）；100ms 守护把假窗口置 TOPMOST 并保持隐藏；消息转发带注册消息过滤 + 重入保护（防退出广播 TaskbarCreated 触发栈溢出）。移植自 C# Orbit / ManagedShell |
| `src/tray.rs` | 托盘镜像数据层：**优先消费 `trayhost` 的托管图标表**（`attach_host`/`poll_hosted`，直接拿 `NIM_MODIFY(hIcon=0)` 的灭帧做闪烁同步，不再 PrintWindow 采样像素）；未托管时退回 UIA + PrintWindow（`NotifyItemIcon` 枚举、exe 大图标 + 相似度校验）。图标来源统一走 `appicon`；只提供数据与状态，不绘制、不持有窗口 |
| `src/traymsg.rs` | 托盘**回调消息转发**：托管模式下目标（`hWnd/uID/uCallbackMessage/uVersion/GUID`）直接来自 `trayhost::Icon`；旧路径从 explorer 的 `TRAYDATA`（`TB_GETBUTTON`+`ReadProcessMemory`）或进程内存 `NOTIFYICONDATAW` 挖。按 v0/3/4 编码投 `WM_MOUSEHOVER`/`NIN_POPUPOPEN`/`WM_MOUSEMOVE`/`WM_MOUSELEAVE`/`NIN_POPUPCLOSE`/左键/右键（含 `AllowSetForegroundWindow` + `NIN_SELECT`）|
| `src/flyout.rs` | **悬停弹窗重定位**：应用悬停时自弹的预览小窗（微信 tool window）会落在旧托盘角（实测左上角 (-24,-24)），后台线程按 exe 名找兄弟进程（双开时卡片进程 ≠ 图标宿主）、`SetWindowPos` 摆到 Dock 上方并守位。**不按"抢前台"排除**（微信卡片自身抢前台）、generation 按图标 hwnd 分组（两实例互不取消）、窗口销毁重建后重新锁定。移植自 C# `HoverFlyoutRelocator` 并针对微信双开加固 |
| `src/appicon.rs` | 托盘图标的高分辨率来源：读 `HKCU\Control Panel\NotifyIconSettings\*` 的 `InitialTooltip`/`ExecutablePath` 建"名字 → exe"表（进程名兜底），再从 exe 取 256px 图标；表与图标都带缓存 |
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

## 文档

| 文件 | 职责 |
|---|---|
| `docs/FILEMAP.md` | 本文件，文件索引 |
| `docs/ARCHITECTURE.md` | 架构总览、widget/托盘/Dock 设计、已知限制、路线图 |

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
