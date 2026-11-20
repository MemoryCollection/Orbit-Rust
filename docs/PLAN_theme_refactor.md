# Orbit UI 主题系统重构 · 开发计划

> 目标：删除原有 6 套风格，改为三套新风格（每套深色 + 浅色共 6 个主题），
> 全界面统一切换；并让「胶囊透明度滑条 100% = 完全不透明」。
> 本文档供后续执行模型 / 开发者照做，已把当前工作区现状、已完成项、待办项、关键坑全部写清。

---

## 0. 需求原文（用户）

1. 删除原有风格。
2. 透明度调整 100% 时 = 不透明。
3. 开发 **Calm UI（平静极简）**：低饱和柔和色系、超大留白、弱化阴影/装饰、字体层级干净、少动效、降低视觉压力。（有参考图）
4. 开发 **Glassmorphism 2.0（改良玻璃拟态）**：克制版毛玻璃，低模糊、高对比度、细描边，不再满屏糊。
5. 开发 **Material 3**：大圆角、动态取色、卡片分层、弹簧动效、涟漪反馈，组件规范完整。
6. 所有风格都添加**深色和浅色模式**。
7. 所有界面 UI 风格统一切换。

---

## 1. 项目背景（执行前必读）

- 项目：`D:/DevTools/Repos/Orbit-Rust`，Rust + 原生 Win32 + egui（软件光栅化）的桌面 Dock / 胶囊悬浮栏。
- 主界面由**多个独立的分层窗口**（`UpdateLayeredWindow`）组成：
  - 顶部胶囊 `ORBIT_CAPSULE`（时钟 + 天气/邮箱/剪贴板模块）
  - 分离模块条 `ORBIT_WIDGETBAR`
  - 音乐条（歌词）
  - 底部 Dock `ORBIT_DOCK`
  - 天气展开面板
  - 设置页 `ORBIT_CAPSULE` 的独立 eframe 窗口（懒启动，首次右键才起线程）
- **核心约束：逐像素 alpha 分层窗口，拿不到桌面像素 → 无法做真背景模糊**。
  所有"毛玻璃"只能靠半透明底 + 内高光 + 细亮边模拟。
- 发光/光晕**只能向内画**（窗口尺寸 == 形状尺寸，向外会被裁）。

---

## 2. 主题系统架构（现状，已存在）

- `src/theme.rs`：`Theme` 结构体 + 内置主题数组 `ALL` + `cur()` / `set_index()` / `index_of()` / `index()`。
- `Theme` 字段（**不要增删字段**，绘制代码全部按字段名引用）：

| 分组 | 字段 |
|------|------|
| 元信息 | `key` `name` `dark` `glow` |
| 胶囊/通用文字 | `pill_bg` `text` `text_dim` `text_body` `icon_dim` `divider` `hover` |
| 展开面板 | `panel_bg` `panel_border` `accent` |
| Dock | `dock_edge` `dock_glow` `dock_hover` `dock_hover_edge` `dock_desktop` `dock_desktop_hover` `icon_backing` |
| 提示条 | `tip_bg` `tip_text` `pin` |
| 天气语义色 | `sun` `moon` `rain` `cloud` |
| 圆角 | `dock_radius` `panel_radius` `tip_radius` |

- 颜色格式一律 `0xAARRGGBB`。
- 热切换机制：`CUR: AtomicUsize` 存下标，`theme::cur()` 每帧零成本读 `'static` 引用；
  各窗口靠 `settings::version()` 轮询感知主题变化后重绘。
- 切换入口：`settings.rs` 的 `set_theme_index(i)`（改内存 + save + bump）。

---

## 3. 当前工作区状态（重要）

### 3.1 已完成但【未提交】的功能（前两轮遗留，勿重复做）
`git status` 显示 18 个文件有未提交改动，包括：
- 程序图标四角透明化 + 重生成 `assets/orbit.ico`（`appicon.rs`、`tools/icon/make_icon.py`）。
- 胶囊/Dock 图标缩放滑条 + 重置（`settings.rs`、`settingsui.rs`、`dock.rs` 等）。
- 托盘图标缩小 + 放大清晰度（`tray.rs`、`render.rs` 的 `capture_icon_rgba` / `fit_icon`）。
- 胶囊去分割线、邮箱/剪贴板矢量图标（`egui_soft.rs::widget_icon`、`pillui.rs`、`widgetbarview.rs`）。
- Dock「返回桌面」按钮从细长条改成圆角显示器图标按钮（`dockview.rs`）。
- `config.rs` 颜色/圆角常量已全部删掉，改为 `cfg::cur().xxx`（106 处调用点）。

> 建议：开发前先 `git add -A && git commit` 或至少确认这些改动是可用的。
> 之前 `git push` 因代理 502 失败，本地已提交 `fcf81fc`，远程可能落后。

### 3.2 本次已做一半的改动（需继续 + 验证）
**`src/theme.rs` 已被重写**，但**尚未编译验证**。当前内容：
- 删除了旧的 6 套：`glass / liquid / mica / paper / neon / terminal`。
- 新增 6 个主题（`ALL` 数组）：

| key | name | dark |
|-----|------|------|
| `calm_light` | 平静 · 浅色 | false |
| `calm_dark` | 平静 · 深色 | true |
| `glass2_light` | 玻璃 2.0 · 浅色 | false |
| `glass2_dark` | 玻璃 2.0 · 深色 | true |
| `m3_light` | Material 3 · 浅色 | false |
| `m3_dark` | Material 3 · 深色 | true |

- 主题内部测试已更新（`each_style_has_both_modes` 等）。

### 3.3 尚未做的（本次计划核心待办）
见第 4 节。

---

## 4. 待办任务清单（按顺序执行）

### 任务 A：让 `theme.rs` 重写通过编译 + 测试 —— ✅ 已完成
- [x] `cargo test theme` → 4 个 theme 测试全绿（`keys_are_unique_and_findable` / `each_style_has_both_modes` / `set_index_clamps` / `theme_values_are_sane`）。
- [x] `cargo test` 全量 → 15/15 通过，无破坏。
- 结论：`theme.rs` 重写（删旧 6 套、新增三风格×深浅色 6 主题）**编译与测试均已通过**。

### 任务 B：旧主题 key 迁移 + 设置页文案
- 文件：`src/settings.rs`
- 旧 `settings.txt` 里的 `theme=glass` 等旧 key 已无法被 `index_of()` 反查，会**静默回落默认**。
  需处理：`load()` 里 `theme` 键解析失败时应明确落到一个合理默认（当前 `index_of` 返回 `None` 时
  `tkey` 保持 `cur().key`，即不覆盖——行为可接受，但建议确认）。
- `append_missing()` 里 `"theme"` 的注释行还是旧的 6 套 key 说明，需改成新的 6 套：
  ```
  # 外观主题：calm_light/calm_dark（平静）/ glass2_light/glass2_dark（玻璃 2.0）/
  # m3_light/m3_dark（Material 3）
  ```
- `BODY`（`ensure_file` 用）里若也写了旧 theme 说明，一并更新。

### 任务 C：透明度 100% = 完全不透明（精确改点）

**现状（已核实）：**
- `src/window.rs:416-422`：`pill_alpha` 的处理是 `if alpha < 0.999 { 整体乘 alpha }`，
  即 `alpha=1.0` 时**保持主题原始底色**（半透明主题如 glass2 底色 alpha=0xC0 仍是半透明）。
- 底色绘制点共 **4 处**，全部用 `argb(cfg::cur().pill_bg)`：
  | 文件 | 行号 | 窗口 |
  |------|------|------|
  | `src/pillui.rs` | `215` | 顶部胶囊底 |
  | `src/dockview.rs` | `156` | 底部 Dock 底 |
  | `src/widgetbarview.rs` | `91-95` | 分离模块条底 |
  | `src/musicbarview.rs` | `71-75` | 音乐条底 |
- 天气面板用 `panel_bg`（`panelview.rs:96/241/316`），是独立窗口、**不受 pill_alpha 影响**，无需处理。
- 工具函数已存在：`egui_soft::with_alpha(c: u32, a: f32) -> u32`（`src/egui_soft.rs:449`）。

**推荐方案（改一处，4 点复用）：**
在 `src/config.rs` 新增一个辅助函数（`config.rs` 已有 `pub use crate::theme::cur;`，追加即可）：
```rust
/// 考虑"透明度滑条"后的胶囊/条底色：
/// 滑条 100%（>=0.999）时强制实底（alpha=0xFF），否则保持主题半透明底色。
pub fn pill_bg_effective() -> u32 {
    let bg = crate::theme::cur().pill_bg;
    if crate::settings::pill_alpha() >= 0.999 {
        (0xFF << 24) | (bg & 0x00FF_FFFF)
    } else {
        bg
    }
}
```
然后把这 4 处 `argb(cfg::cur().pill_bg)` 改为 `argb(cfg::pill_bg_effective())`：
- `src/pillui.rs:215`
- `src/dockview.rs:156`
- `src/widgetbarview.rs:94`
- `src/musicbarview.rs:74`

**注意事项：**
- 只提亮**底色** alpha，文字/图标颜色（`text` / `text_body` / `icon_dim` 等）**不动**，
  文字必须保持可读。
- 半透明主题（glass2/m3）在滑条 <100% 时仍保持半透明，只有 100% 才实底——这正是需求语义。
- 若想更彻底（Dock 的内描边 `dock_edge`、反光 `dock_glow` 也随实底变），可一并判断，
  但**默认不做**，避免实底时亮边/高光显得突兀；先交付底色实底即可。

### 任务 D：设置页主题选择器确认
- `src/settingsui.rs` 的色块选择器用 `crate::theme::ALL` 遍历，**自动跟随新 6 套**，无需改逻辑。
- 需确认：色块下方显示 `cfg::cur().name` 正常；选中框、悬停名正常。
- 色块用「模拟壁纸中性底 + 叠 `pill_bg`」的画法（`settingsui.rs:468-486`）——新主题半透明底
  仍能区分，理论上 OK。

### 任务 E：Material 3 的「弹簧动效 + 涟漪反馈」落点
- 说明：本项目是**逐帧软件光栅化的分层窗口**，没有动画循环，也没有真正的"点击涟漪"回调通道。
- 现实可落地范围（按成本递增）：
  1. **圆角/分层/取色**：已通过 theme 字段覆盖（大圆角、卡片分层靠 panel_border/panel_bg 对比）。✅ 已做
  2. **涟漪反馈**：Dock 图标悬停已有「圆形药丸底 + 描边」反馈（`dockview.rs`），可视为低配涟漪。
     若要真涟漪，需在 Dock/胶囊的 hit 处理里加「按下 → 扩散圆」的短期动画（需新增定时重绘 + 状态），
     工作量大、收益低，**建议列为可选/二期**。
  3. **弹簧动效**：悬停图标放大已有 `DOCK_HOVER_GROW`（瞬时缩放，非弹簧）。真弹簧需要缓动 + 帧循环，
     **建议二期**。
- 结论：主题系统层面已能表达 M3 的静态特征；动效类建议在计划里标注「二期」，先交付静态风格统一。

### 任务 F：编译 + 实跑截图验证（关键）
- [ ] `cargo test`（theme 4 个测试 + settings 若干）→ 全绿。
- [ ] `cargo build --release`
- [ ] 逐套主题实跑，抓窗口截图验证（已有工具 `tools/_shots/grab_window.py`，抓 `ORBIT_CAPSULE` / `ORBIT_DOCK` / `ORBIT_WIDGETBAR`）：
  1. 深浅色各 3 套共 6 套，检查胶囊文字对比度、图标衬底、Dock 图标清晰度、面板配色。
  2. 浅色主题下关闭按钮（设置页）斜线是否可见（用 `cur().text`，浅色下应为深色）。
  3. 透明度拉到 100%，确认胶囊/Dock 为实底不透明。
  4. 切换主题后所有窗口（胶囊/模块条/音乐条/Dock/面板/设置页）统一切换。
- 启动方式：后台运行 `./target/release/orbit.exe`（`run_in_background: true`）；
  设置页打开：`FindWindowW("ORBIT_CAPSULE")` + `PostMessageW(WM_RBUTTONUP=0x0205)`。

> 附：任务 A（theme.rs 编译验证）可先单独跑 `cargo test theme` / `cargo check`，
> 确认重写无破坏后再做 B/C，最后统一 build + 截图。

---

## 5. 关键坑（务必注意）

1. **半透明主题底色的 alpha 是设计的一部分**：glass2/m3 的 `pill_bg` 自带 alpha（如 0xC0）。
   只有「透明度滑条 100%」这个用户显式动作才应把它提到实底；普通情况下保持半透明。
2. **浅色主题必须给 Dock 图标垫白衬底**（`icon_backing` 非 0），否则彩色图标压在浅底发脏。
   新浅色主题已设 `icon_backing`，勿漏。
3. **生僻 Unicode 字形不可用**：所有"图标"都不要用 U+2715 之类（雅黑无字形）。
   关闭按钮是 painter 自绘斜线（`settingsui.rs::close_button`），不要改回字符。
4. **egui Slider 宽度**由 `ui.spacing_mut().slider_width` 控制，`add_sized` 管不到。
5. **主题热切换**不重建窗口，靠 `version()` 轮询；手改 `settings.txt` 的 `theme=` 不会热加载，
   只有设置页点选才生效（或重启）。
6. **霓虹发光已弃用**：新 6 套主题 `glow` 全为 `false`；`glow_edges` 代码可保留但不会触发。
7. **`ALL` 数组下标 = 设置页顺序**，测试 `set_index_clamps` 断言了 `cur().key == "calm_light"`，
   改动顺序/名称时要同步改测试。

---

## 6. 交付标准

- [ ] `cargo test` 全绿（theme 测试 4 个 + settings 测试若干）。
- [ ] `cargo build --release` 成功。
- [ ] 6 套主题截图确认可读性、统一性。
- [ ] 透明度 100% 实底验证通过。
- [ ] 设置页切换任意主题，所有窗口统一切换。
- [ ] （建议）`git commit` 落盘，并尝试 `git push`（代理可能 502，失败则记录）。
