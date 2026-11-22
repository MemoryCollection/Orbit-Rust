# Orbit 交接文档（Handover）

> 交接日期：2026-11-21。供后续开发者 / 执行模型快速接手。
> 阅读顺序：本文 → `docs/FILEMAP.md`（文件索引，先查表再读代码）→ `docs/ARCHITECTURE.md`（设计细节与踩坑记录）。

## 1. 项目概览

**Orbit**：Windows 11 桌面增强悬浮栏（Rust + 原生 Win32 + egui 软件光栅化，无 WebView、低内存）。

- 顶部胶囊（时钟/日期 + 可分离模块条 + 展开面板）、音乐条（旋转封面 + 歌词）、底部 Dock（钉选 + 运行中应用 + 托盘完整托管）、桌面右上角天气卡片、右下角通知横幅、开始菜单接管（Win 键）、托盘闪烁提示音 / 微信消息卡片。
- 核心约束：所有分层窗口依赖 `WS_EX_NOACTIVATE` + 逐像素 alpha + 圆角命中，绘制链路统一为 **egui 布局 → `egui_soft` 软件光栅化 → 预乘 RGBA → `render::present()`（UpdateLayeredWindow）**，不能换成 eframe 原生窗口。
- 托盘是"完整托管"架构（假 `Shell_TrayWnd` + `TaskbarCreated` 重注册 + `WM_COPYDATA` 图标表），这是隐藏原生任务栏 + 悬停弹卡片 + 稳定镜像三者同时成立的前提，**不要回退到 UIA 镜像方案**。

## 2. 构建与运行

```bash
cargo run --release                      # 默认接管（隐藏）原生任务栏，退出自动恢复
cargo run --release -- --show-taskbar    # 保留原生任务栏（调试）
cargo run --release -- --restore-taskbar # 进程被强杀、任务栏未归位时手动救
cargo check --all-targets                # 编译检查（本次清理后应为 0 告警）
cargo test                               # 单测（settings.rs 内含主题迁移等测试）
ORBIT_TRACE=1 cargo run --release        # 详细日志写 <exe>/orbit.log
```

- 打包：仓库根 `打包exe.bat`；应用图标由 `build.rs`（winresource）+ `tools/icon/make_icon.py` 生成。
- 数据全部便携式落在 `<exe>/data/`（settings.txt、mail.txt、clipboard.txt 等），不写注册表数据、不碰安装目录之外（开机自启动的 Run 键 / 启动文件夹 orbit.lnk / 计划任务 Orbit 除外，见 `guard.rs`）。

## 3. 当前状态（交接时点）

- 最近提交到 `3145eb1 fix(flyout): 弹窗/菜单移位只在鼠标位于 Dock 内时执行`。
- 交接前的工作区包含一批**未提交**的改动（本次已先做 checkpoint 提交再清理，历史可回溯）：
  - 新增模块：`appmsg.rs`（设置即时通知）、`banner.rs`（右下角横幅）、`guard.rs`（单实例/开机自启/退出守护/电源动作）、`startmenu.rs`（Win 键开始菜单）、`weathercard.rs`（桌面天气卡片）、`wechatmsg.rs`（微信消息 UIA 读取）、`theme.rs`（主题系统）。
  - 天气已从胶囊/面板移出，改为桌面右上角常驻卡片（`weathercard.rs`），胶囊里不再有天气 widget。
  - 设置页改为分组导航 + 「应用/还原」编辑会话（`settingsui.rs`）。
- `docs/FILEMAP.md` 已按铁律同步补登 `guard.rs` / `startmenu.rs` / 本文件。

## 4. 本次清理记录（删除过期代码）

依据：`cargo check --all-targets` 的 27 条死代码告警，逐条人工核实引用关系后删除；**编译与单测验证通过后落盘**。

### 已删除

| 位置 | 删除内容 | 原因 |
|---|---|---|
| `config.rs` | `WidgetPos` 枚举及 `SHOW_WEATHER` / `WEATHER_POS` / `WEATHER_SLOT` / `CLIPBOARD_POS` / `CLIPBOARD_SLOT` / `MAIL_SLOT` / `MAIL_POS` / `WIDGET_GLYPH_INK` / `FLASH_ICON` | 旧的「左1/右1」编码机制已被 `module_order` + `*_mode` 设置取代（ARCHITECTURE「模块布局」一节）；闪烁图标尺寸改由代码内常量决定 |
| `pillui.rs` | `PillEvent::FlashClick` 变体；`View` 的 `hovered_widget` / `hovered_flash` 字段；`Painted` 同名透传字段；189 行多余 `mut` | `paint()` 从不产出 FlashClick（闪烁图标整窗穿透，点击走 Dock）；hover 状态由 egui 内部管理，这两个字段只写不读 |
| `window.rs` | `App::click_flash`、`App.hovered_widget/hovered_flash` 字段及其校验/回传回路、`PillEvent::FlashClick` 分支 | 上项的调用侧，随事件变体一起失效 |
| `render.rs` | `FontKind::Latin` 变体及字体映射臂；`FlashView.px` 字段 | 无构造点 / 无读取点 |
| `theme.rs` | `dock_hover` / `dock_hover_edge` / `dock_desktop` / `dock_desktop_hover` 字段及 6 套主题的赋值 | GDI+ 绘 Dock 时代的遗留，Dock/悬停底已全部由 egui 绘制 |
| `tray.rs` | `TrayMirror::report_placements` | 已被 `dock.rs` 直接构造 `trayhost::Placement` + `trayhost::set_placements` 取代 |
| `trayhost.rs` | `is_active()` | 无引用 |
| `mailview.rs` | `hide()` | 无引用（阅读窗收起走销毁路径） |
| `settings.rs` | `egui_theme()`、`set_wechat_sound()` | 无引用（`wechat_sound` 的读/写盘/设置键保留，仅此 setter 无人调用） |
| `banner.rs` | `default_ms()` | 调用点已改为直接 `settings::banner_secs()` |
| `dock.rs` | 460 行嵌套多余 `unsafe`；`winlist_pos` 里的死变量 `board_t` / `board_b` | 编译告警清理 |
| `settingsui.rs` | 952 行多余 `mut` | 编译告警清理 |
| `docs/EGUI_MIGRATION_PLAN.md`、`docs/PLAN_theme_refactor.md` | 两份已执行完毕的历史计划文档 | egui 迁移与主题重构均已落地（见 git 历史），内容已并入 ARCHITECTURE.md；文档本体从 git 历史可随时找回 |

### 有意保留（勿当死代码删除）

- `trayhost::Host.state` / `snapshot()` / `generation()`：被 `tools/trayhostcheck`（`#[path]` 引入产品模块的验证 harness）使用；`cargo check` 不覆盖 tools 子 crate，故会误报。
- `settings` 中 `legacy_theme_key` 及其迁移测试：老主题 key → 新 key 的兼容迁移，属功能代码。

## 5. 验证结果

- `cargo check --all-targets`：0 告警（清理前 27 条）。
- `cargo test`：全部通过（含 `legacy_theme_keys_map_to_new_styles` 等迁移测试）。

## 6. 开发约定与注意事项

1. **FILEMAP 铁律**：新增/删除/重命名文件或职责变化，必须同步更新 `docs/FILEMAP.md`。
2. 托盘相关调试用 `tools/traytest`（不要用微信测，多开会混淆）；各 tools 子 crate 是独立 Cargo 项目，需单独 `cargo run`。
3. `data/settings.txt` 改文件即时生效（500ms mtime 轮询兜底），设置页改值经 `appmsg::notify_applied()` 即时通知；两路共用 `settings::version()` 判断重绘。
4. 分层窗口常见坑（预乘 RGBA、ppp 经 `RawInput.viewports` 下发、命中矩形乘 scale 等）见 ARCHITECTURE.md 开头的「常见坑」块——改渲染前先读。
5. 退出热键 `Ctrl+Alt+O`；强杀后任务栏未归位用 `--restore-taskbar`；守护进程会救回任务栏并**自动复活 orbit**（连续最多 3 次，成功清零计数；次数用尽才重启 explorer，见 `guard.rs`）。

## 7. 已知限制与路线图

见 `docs/ARCHITECTURE.md` 末尾两节（微信卡片需一次「手动拿 UIA」、托盘托管依赖 TaskbarCreated 重注册、歌词只显示原文一行、Dock 无缩略图/多屏等；路线图含托盘溢出区、天气城市手动指定、邮件多账号等）。
