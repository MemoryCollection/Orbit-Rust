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

### 3.1 已完成的功能（已提交 `9037447`）
前两轮遗留的改动已经全部落进提交 `9037447`（工作区干净）：
- 程序图标四角透明化 + 重生成 `assets/orbit.ico`（`appicon.rs`、`tools/icon/make_icon.py`）。
- 胶囊/Dock 图标缩放滑条 + 重置（`settings.rs`、`settingsui.rs`、`dock.rs` 等）。
- 托盘图标缩小 + 放大清晰度（`tray.rs`、`render.rs` 的 `capture_icon_rgba` / `fit_icon`）。
- 胶囊去分割线、邮箱/剪贴板矢量图标（`egui_soft.rs::widget_icon`、`pillui.rs`、`widgetbarview.rs`）。
- Dock「返回桌面」按钮从细长条改成圆角显示器图标按钮（`dockview.rs`）。
- `config.rs` 颜色/圆角常量已全部删掉，改为 `cfg::cur().xxx`（106 处调用点）。
- `theme.rs` 三套风格 × 深浅色 6 主题。

> 远程可能落后：之前 `git push` 因代理 502 失败过。

### 3.2 主题系统现状（2026-11-20 收尾）
**`src/theme.rs` 已编译通过并有测试保护**。当前 `ALL` 数组：

| key | name | dark |
|-----|------|------|
| `calm_light` | 平静 · 浅色 | false |
| `calm_dark` | 平静 · 深色 | true |
| `glass2_light` | 玻璃 2.0 · 浅色 | false |
| `glass2_dark` | 玻璃 2.0 · 深色 | true |
| `m3_light` | Material 3 · 浅色 | false |
| `m3_dark` | Material 3 · 深色 | true |

- 旧的 `glass / liquid / mica / paper / neon / terminal` 已删除，老 `settings.txt` 由
  `settings::legacy_theme_key()` 就近映射（见任务 B）。
- 透明度滑条 100% = 实底，由 `config::pill_bg_effective()` 统一处理（见任务 C）。

### 3.3 尚未做的
- Material 3 的**弹簧动效 / 真涟漪**（任务 E，已定二期）。
- 设置页本体视觉人工确认（任务 D，环境抓不到 GL 窗口）。

---

## 4. 待办任务清单（按顺序执行）

### 任务 A：让 `theme.rs` 重写通过编译 + 测试 —— ✅ 已完成
- [x] `cargo test theme` → 4 个 theme 测试全绿（`keys_are_unique_and_findable` / `each_style_has_both_modes` / `set_index_clamps` / `theme_values_are_sane`）。
- [x] `cargo test` 全量 → 15/15 通过，无破坏。
- 结论：`theme.rs` 重写（删旧 6 套、新增三风格×深浅色 6 主题）**编译与测试均已通过**。

### 任务 B：旧主题 key 迁移 + 设置页文案 —— ✅ 已完成
- [x] `src/settings.rs` 新增 `legacy_theme_key()`：旧 6 个 key 就近映射到新主题，
      深浅色不反（`glass`/`liquid`→`glass2_dark`、`mica`→`glass2_light`、
      `paper`→`calm_light`、`neon`→`m3_dark`、`terminal`→`calm_dark`）。
      `load()` 的 `theme` 分支：先 `index_of()`，失败再走映射，都不中就保持当前主题。
- [x] `BODY`（新建文件用）补上 `theme=calm_light` 与新的 6 套说明；
      `append_missing()` 的 `"theme"` 注释行同步改成新 6 套。
- [x] 文件头注释补 `theme` 键说明 + `pill_alpha` 100% 的语义。
- [x] 新增测试 `legacy_theme_keys_map_to_new_styles`（断言映射存在、深浅色一致）。
- 实测：把 `settings.txt` 写成 `theme=glass` 启动，实际渲染为 `glass2_dark`
      （胶囊底色像素 `#16181E`），迁移生效。

### 任务 C：透明度 100% = 完全不透明（精确改点）—— ✅ 已完成

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

**实际落地（与上面方案一致）：**
- `config.rs`：`pill_bg_effective()` + 纯函数 `opaque(c)`（只提 alpha 字节），
  配了单元测试 `opaque_keeps_rgb_and_forces_alpha`。
- 4 处底色绘制点全部改为 `argb(cfg::pill_bg_effective())`：
  `pillui.rs` / `dockview.rs` / `widgetbarview.rs` / `musicbarview.rs`。
- 实测像素：6 套主题在 100% 时底色 alpha 均为 255（RGB 与主题一致）；
  玻璃 2.0 深色拉到 90% 时预乘 RGB 为 `(15,16,21)`（≈ 0xC0×0.9），半透明照旧。
- 副作用（已知、可接受）：Dock 与音乐条**不受滑条数值调节**（`pill_alpha` 只整帧压胶囊
  与模块条），但它们也吃 100% 这条规则——所以玻璃主题在默认 100% 下是实底，
  想看毛玻璃效果需把滑条往 100% 以下拉一点。

### 任务 D：设置页主题选择器确认 —— ✅ 逻辑已确认，视觉待人工看一眼
- `src/settingsui.rs` 的色块选择器用 `crate::theme::ALL` 遍历，**自动跟随新 6 套**，无需改逻辑。
- 已确认：色块下方显示 `cfg::cur().name`；选中框用 `t.accent`、悬停名 `t.name`；都按主题取色。
- 色块用「模拟壁纸中性底 + 叠 `pill_bg`」的画法（`settingsui.rs:468-486`）——新主题半透明底
  仍能区分。
- **注意**：设置页是 eframe/GL 渲染的普通窗口，`PrintWindow` 只能拿到原生标题栏、
  `ImageGrab` 在无交互桌面时会失败 → 本轮**无法截图**验证设置页视觉，需本机人工点一下。

### 任务 E：Material 3 的「弹簧动效 + 涟漪反馈」落点（标注二期，本轮不做）
- 说明：本项目是**逐帧软件光栅化的分层窗口**，没有动画循环，也没有真正的"点击涟漪"回调通道。
- 现实可落地范围（按成本递增）：
  1. **圆角/分层/取色**：已通过 theme 字段覆盖（大圆角、卡片分层靠 panel_border/panel_bg 对比）。✅ 已做
  2. **涟漪反馈**：Dock 图标悬停已有「圆形药丸底 + 描边」反馈（`dockview.rs`），可视为低配涟漪。
     若要真涟漪，需在 Dock/胶囊的 hit 处理里加「按下 → 扩散圆」的短期动画（需新增定时重绘 + 状态），
     工作量大、收益低，**建议列为可选/二期**。
  3. **弹簧动效**：悬停图标放大已有 `DOCK_HOVER_GROW`（瞬时缩放，非弹簧）。真弹簧需要缓动 + 帧循环，
     **建议二期**。
- 结论：主题系统层面已能表达 M3 的静态特征；动效类建议在计划里标注「二期」，先交付静态风格统一。

### 任务 F：编译 + 实跑截图验证（关键）—— ✅ 已完成
- [x] `cargo test` → **17/17 通过**（theme 4 个 + config 1 个 + settings 6 个 + 其它）。
- [x] `cargo build --release` 成功。
- [x] 6 套主题实跑截图（`tools/_shots/t_<key>_pill.png` / `_dock.png`，对照图 `_themes_sheet.png`）：
  1. 深浅色各 3 套：胶囊文字对比度、图标衬底、Dock 图标清晰度、天气面板配色均正常。
     浅色主题 Dock 图标有白衬底（`icon_backing`），深色主题无衬底，彩色图标不发脏。
  2. 浅色主题下「返回桌面」按钮（Dock 右端）实测为深灰描边 + 浅灰底，清晰可见。
  3. 透明度拉到 100%：胶囊 / 模块条 / Dock 底色 alpha 全为 255。
  4. 切主题（改 `settings.txt` 的 `theme=` 后等 ~1s）胶囊、模块条、Dock 同帧一起变，
     天气面板同样跟随。
- 无法自动验证：设置页本体的视觉（GL 窗口抓不到，见任务 D）。
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
5. **主题热切换**不重建窗口，靠 `version()` 轮询；**手改 `settings.txt` 的 `theme=` 也会热加载**
   （`reload_if_changed()` → `load()` → `theme::set_index()`，实测约 1s 生效，6 套都验证过）。
   注意落盘的文件可能仍留着旧 key（只有下次 `save()` 才改写），不影响运行。
6. **霓虹发光已弃用**：新 6 套主题 `glow` 全为 `false`；`glow_edges` 代码可保留但不会触发。
7. **`ALL` 数组下标 = 设置页顺序**，测试 `set_index_clamps` 断言了 `cur().key == "calm_light"`，
   改动顺序/名称时要同步改测试。

---

## 6. 交付标准

- [x] `cargo test` 全绿 → **17/17**（theme 4 + config 1 + settings 6 + clock/json/lyric 6）。
- [x] `cargo build --release` 成功（1m23s，仅 dead_code 警告）。
- [x] 6 套主题截图确认可读性、统一性（`tools/_shots/t_*_pill.png`、`_dock.png`、`_themes_sheet.png`）。
- [x] 透明度 100% 实底验证通过（6 套底色 alpha = 255；90% 时预乘 RGB 按 0xC0×0.9 变暗）。
- [x] 切换主题后所有窗口统一切换（胶囊 / 模块条 / Dock / 天气面板实测同帧跟随）。
- [ ] 设置页本体视觉：环境抓不到 GL 窗口，需人工确认（逻辑无改动，色块遍历 `theme::ALL`）。
- [ ] `git commit` 落盘并尝试 `git push`（代理可能 502，失败则记录）。
