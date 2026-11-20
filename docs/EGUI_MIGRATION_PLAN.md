# Orbit UI 全量迁移 egui —— 执行计划

> 目标：把剩余所有 GDI+ 自绘 UI 全部改为 egui 绘制，最终删除 `render.rs` 的 GDI+ 绘制代码，只保留图标捕获等非绘制能力。
>
> 本计划供其它模型/开发者按步骤执行。每个阶段都可独立编译验证，失败不阻塞后续阶段。

---

## 0. 现状盘点（执行前先读）

### 已完成 egui 化的部分
| 模块 | 文件 | 现状 |
|---|---|---|
| 设置页 | `src/settingsui.rs` | ✅ 已是 egui/eframe（独立线程 + glow 后端），无需改动 |
| 顶部胶囊 | `src/pillui.rs` | ✅ 已是 egui 软件后端（方案 C：egui 布局 → epaint 网格 → 自写光栅化 → `render::present` 上屏） |

### 仍是 GDI+ 自绘的部分（本次要迁）
全部集中在 `src/render.rs`（约 1808 行），被以下调用方使用：

| 调用方 | 用到的 render 函数 | UI 内容 |
|---|---|---|
| `dock.rs` | `dock_size` / `draw_dock` / `draw_tooltip` / `to_premultiplied_bgra` | 底部 Dock（应用图标 + 托盘图标 + 返回桌面条） |
| `panel.rs` | `panel_size` / `weather_panel_size` / `draw_panel` / `draw_weather_panel` / `to_premultiplied_bgra` | 展开面板（列表 + 天气宽卡片） |
| `widgetbar.rs` | `widget_bar_w` / `draw_widget_bar` / `to_premultiplied_bgra` | 分离出去的模块条 |
| `musicbar.rs` | `music_bar_fixed_w` / `music_bar_w` / `cover_box` / `draw_music_bar` / `to_premultiplied_bgra` | 音乐条（旋转封面 + 歌词） |
| `window.rs` | `present` / `premultiplied_rgba_to_bgra` / `cover_px` | 胶囊上屏（present 复用） |
| `tray.rs` | `capture_icon_rgba` | 托盘图标像素捕获（**非绘制，保留**） |
| `app.rs` | `startup` / `shutdown` | GDI+ 生命周期 |

### 关键结论（决定迁移策略）
1. **这些窗口都是"分层窗口"（`WS_EX_LAYERED` + `UpdateLayeredWindow`），依赖逐像素 alpha 透明**，因此**不能直接用 eframe 原生窗口**（eframe 是普通窗口，不支持逐像素透明）。这正是当初"只切设置页、不动其它 UI"的决策依据。
2. **pillui.rs 已经给出了正确答案**：egui 只负责"布局 + 生成 epaint 网格"，再**自写软件光栅化**成预乘 RGBA，交给 `render::present()` 上屏。**迁移的核心就是把 pillui 这套模式复制到 dock/panel/widgetbar/musicbar 四个模块。**
3. `render.rs` 里 `capture_icon_rgba` / `resize_rgba` / `decode_cover` / `rotate_cover` / `sample_rgb` / `blend_over` 等是**图像处理**（非 UI 绘制），**必须保留**。

---

## 1. 总体策略

**统一模式（复用 pillui 已验证的"软件后端"）：**

```
数据快照 (View)
   → egui 立即模式布局（CentralPanel + 布局）
   → ctx.tessellate() 得到 epaint 网格
   → 自写三角形光栅化（fill_tri，现成）
   → 预乘 RGBA 缓冲
   → render::present() 上屏（UpdateLayeredWindow）
```

**抽一个公共后端 `src/egui_soft.rs`**，把 pillui 里的通用能力抽出来，供 4 个模块共用，避免复制粘贴：

- `SoftBackend` 结构体：持有 `egui::Context`、`HashMap<TextureId, TexData>`、字体安装、`pixels_per_point` 管理。
- `rasterize(ctx, shapes, ppp, w, h) -> Vec<u8>`：把 epaint 网格光栅化成预乘 RGBA（`fill_tri` 逻辑搬过来）。
- `tex_upload(tex_id, image)` / `tex_lookup(tex_id)`：纹理管理（`TexData` / `image_to_tex` / `sample_tex` 搬过来）。
- `argb(u32) -> Color32`、`install_fonts(ctx)`、`make_input(...) -> RawInput`：小工具。
- `soft_image(handle) -> ColorImage`：把"RGBA 像素块"上传为 egui 纹理（dock 图标、托盘图标、封面等像素数据都要走这里）。

> 注意：epaint 网格光栅化涉及 fill_tri、纹理采样、预乘合成，代码约 300 行。pillui 已跑通，抽成公共模块即可。

---

## 2. 分阶段迁移（每阶段独立可编译、可回退）

### 阶段 1：抽取公共软件后端 `src/egui_soft.rs`（纯重构，行为不变）
- 从 `pillui.rs` 抽出 `TexData` / `image_to_tex` / `sample_tex` / `fill_tri` / `argb` / `install_fonts` 到 `egui_soft.rs`。
- `pillui.rs` 改为 `use crate::egui_soft::*;`，删除重复代码。
- 编译 + 运行验证胶囊行为不变（这是回归基线）。
- **验收**：`cargo check` 通过，胶囊渲染/点击正常。

### 阶段 2：Dock 迁移（`dock.rs`）
1. 在 `Dock` 结构体里加 `soft: egui_soft::SoftBackend` 字段。
2. 新建 `src/dockview.rs`（或在 dock.rs 内加一个 `paint_dock(view) -> DockRender` 函数，返回 `rgba` + 命中矩形 + 托盘矩形 + 返回桌面矩形），用 egui 布局：
   - 底板：`p.rect_filled(..., CornerRadius(DOCK_RADIUS), BG_ARGB)`
   - 应用图标：`ui.allocate_exact_size(DOCK_SLOT)` + `p.image(tex_id, rect)`；图标像素先 `soft_image()` 上传。
   - 分隔线：`p.rect_filled`（1px 竖线）
   - 托盘图标：同款，圆底 `p.circle_filled`
   - 返回桌面细长条：`p.rect_filled` 竖圆角条
   - 命中矩形从 `Response.rect` 收集（逻辑点），再乘 scale 转物理像素。
3. 尺寸计算：用 egui 字体度量或直接沿用现有 `dock_size` 公式（**建议先沿用公式，减少回归面**）。
4. `draw_dock` 调用点替换为 `dockview::paint`，`to_premultiplied_bgra` 之后 `present`。
5. 验收：Dock 显示、悬停高亮、点击、托盘图标、返回桌面、四个锚位（含竖排）全部正常。

### 阶段 3：模块条迁移（`widgetbar.rs`）
- 结构最简单，跟胶囊 widget 区几乎一样，可大量参考 pillui 的 widget 排布代码。
- 新建 `paint_widget_bar(view) -> PillRender`，egui 布局：圆角底 + 一排 `WidgetItem`（图标 `p.text` + 文本 `p.text`）+ 悬停底。
- 验收：左/右模块条显示、悬停、点击回传、同侧多模块合并、透明度烘焙正常。

### 阶段 4：音乐条迁移（`musicbar.rs`）
- 最复杂（旋转封面）。策略：
  - **封面旋转仍用现有 `render::rotate_cover()`**（纯像素运算，保留），旋转结果 `soft_image()` 上传为 egui 纹理，`p.image()` 画圆形（或 `Mesh` 圆）。
  - 歌词文本 `p.text`；播放/暂停按钮用 `p.rect_filled`（两根竖条 / 三角形近似）。
  - 命中（封面圆内）由 `Response.rect` + 圆内判定，点击回传 WM_TOGGLE/WM_NEXT。
- 验收：旋转封面、歌词、播放/暂停按钮、无封面占位、三个 lyric_position 全部正常。

### 阶段 5：展开面板迁移（`panel.rs`）
- 新建 `paint_panel(view) -> PanelRender`，egui 布局：
  - 圆角底 + 描边（`p.rect_stroke`）
  - 标题/副标题 `p.text`；右上标签页用 `Button::selectable` 或 `p.rect_filled` + 文字
  - 行列表：图标 `p.text` + 标题 + 副标题 + 右侧 value + 固定按钮（★/☆）
  - 天气卡片：左栏实况 + 右栏四天预报 + 刷新按钮，布局照搬现有 `draw_weather_panel` 的坐标节奏
  - 命中区从 `Response.rect` 收集，动作号编码保持 `0..999 行 / 1000+ 固定 / 2000+ 标签页` 不变
- 验收：列表面板、天气卡片、标签页、固定、点击回传动作号全部正常。

### 阶段 6：清理 GDI+ 绘制代码
- 从 `render.rs` 删除已迁移的绘制函数：`draw_dock` / `dock_size` / `draw_tooltip` / `draw_widget_bar` / `widget_bar_w` / `draw_music_bar` / `music_bar_*` / `cover_box` / `draw_panel` / `panel_size` / `draw_weather_panel` / `weather_panel_size` / `draw_string` / `round_rect` / `round_stroke` / `measure_text` / `solid_fill` / `build_mask` / `with_mask` / `mask_sample` / `blit_mask` / `draw_glyph_cell` / `draw_widget_strip` / `scratch_graphics` / `font_ascent` / `create_font` / `ellipsize` 等。
- **保留**：`startup` / `shutdown` / `present` / `capture_icon_rgba` / `resize_rgba` / `decode_cover` / `rotate_cover` / `sample_rgb` / `blend_over` / `swap_rb` / `to_premultiplied_bgra` / `premultiplied_rgba_to_bgra` / `window_icon` / 数据结构（`WidgetItem` / `PanelRow` / `WeatherView` / `WeatherDay` / `FlashView` / `PillShape` 等，这些是跨模块的数据契约，若迁移后不再被 render 引用则挪到对应模块或保留）。
- 从 `Cargo.toml` 移除不再需要的 windows features（`Win32_Graphics_GdiPlus` 等在全部绘制删除后评估；`capture_icon_rgba`/`decode_cover` 仍用 GDI+，故 **GdiPlus feature 仍保留**）。

### 阶段 7：收尾
- 删除 `pillui.rs` 里迁走后重复的逻辑（或让 pillui 也改用 `egui_soft` 后端，见阶段 1 已做）。
- 更新 `docs/ARCHITECTURE.md`、`docs/FILEMAP.md`。
- 全量 `cargo test` + `cargo build --release` + 真机回归（Dock 四锚位、模块条、音乐条、面板、天气卡片、托盘闪烁、透明度、DPI 1.25）。

---

## 3. 关键风险与注意点

1. **逐像素 alpha 透明**：Dock/面板/模块条/音乐条都是分层窗口，必须继续走"egui 软件后端 → 预乘 RGBA → `render::present`"，**不要**试图改成 eframe 原生窗口（会失去透明和点击穿透能力）。

2. **命中矩形坐标系统**：pillui 里命中矩形是"逻辑点 → 乘 scale 转物理像素"。各模块迁移时必须保持一致，否则点击错位（尤其 DPI 1.25）。

3. **图标/封面像素要上传纹理**：egui 画 `p.image` 需要 `TextureId`。所有像素块（应用图标、托盘图标、旋转封面、闪烁图标）都要先 `soft_image()` 上传成 egui 纹理再画。注意纹理缓存与释放（内容 hash 复用）。

4. **变化检测（性能）**：pillui 用 hash 判断"内容没变就跳过光栅化"。Dock/音乐条等也有各自的 `show()` 变化检测（只在变了才重画），迁移时要保留这个优化，否则 70ms/200ms 定时器会空转光栅化。

5. **字体**：egui 默认不含中文字体，`install_fonts`（微软雅黑 + Segoe UI Symbol）必须复用到所有后端，否则中文/符号图标乱码。

6. **颜色**：`argb()` 辅助函数（ARGB u32 → egui Color32）务必复用，所有 `cfg::*_ARGB` 常量语义不变。

7. **动作号协议**（面板 `PIN_BASE`/`TAB_BASE`、模块条 `WM_WIDGET_CLICK`、音乐条 `WM_TOGGLE`/`WM_NEXT`）是跨模块契约，迁移时**编码不变**，只换绘制层。

8. **图标归一化**：胶囊里"符号图标按墨迹面积归一化"（`build_mask`/`draw_glyph_cell`）是 GDI+ 专属逻辑，egui 里改用 `p.text` 直接画字形即可（字体统一后视觉差异可接受，若不满意再补归一化）。这是唯一的视觉退化风险点，需真机确认。

---

## 4. 执行顺序建议（依赖关系）

```
阶段1（抽公共后端）  ← 必做前置
   ↓
阶段2（Dock） 阶段3（模块条）  并行，互不依赖
   ↓
阶段4（音乐条） 阶段5（面板）  并行，互不依赖
   ↓
阶段6（清理 GDI+）  ← 全部完成后
   ↓
阶段7（收尾 + 回归）
```

- 阶段 2~5 都依赖阶段 1 的 `egui_soft`。
- 阶段 2/3 与阶段 4/5 之间无强依赖，但建议按"简单→复杂"顺序推进（模块条最简单，音乐条/面板最复杂）。

---

## 5. 每个阶段的具体改动清单（给执行者）

### 阶段 1
- [ ] 新建 `src/egui_soft.rs`，`pub mod egui_soft;` 加入 main.rs
- [ ] 从 pillui.rs 搬 `TexData`/`image_to_tex`/`sample_tex`/`fill_tri`/`argb`/`install_fonts`/`divider`
- [ ] 新增 `SoftBackend`（Context + tex map + ppp 管理 + `rasterize()` + `soft_image()`）
- [ ] pillui.rs 改为复用 egui_soft，删重复代码
- [ ] `cargo check` + 运行验证胶囊不变

### 阶段 2
- [ ] dock.rs 加 `soft: SoftBackend`
- [ ] 实现 `paint_dock()` 替换 `render::draw_dock`
- [ ] `cargo check` + 真机验证 Dock 四锚位

### 阶段 3
- [ ] widgetbar.rs 加 soft 后端 / 复用
- [ ] 实现 `paint_widget_bar()` 替换 `render::draw_widget_bar`
- [ ] 真机验证模块条

### 阶段 4
- [ ] musicbar.rs 加 soft 后端
- [ ] `rotate_cover` 结果上传纹理 → egui 圆形绘制
- [ ] 真机验证音乐条三位置

### 阶段 5
- [ ] panel.rs 加 soft 后端
- [ ] `paint_panel()` + `paint_weather_panel()`
- [ ] 真机验证面板 + 天气卡片

### 阶段 6
- [ ] 删除 render.rs 已迁移绘制函数（保留图像处理 + present + 数据结构）
- [ ] 评估 Cargo.toml windows features 精简
- [ ] cargo check / test / build --release

### 阶段 7
- [ ] 更新 docs
- [ ] 全量回归 + DPI 1.25 验证

---

## 6. 验收标准（总）

1. `cargo build --release` 成功，无编译错误；`cargo test` 全绿。
2. 胶囊、Dock、模块条、音乐条、面板、天气卡片全部由 egui 绘制（grep 确认 `render::draw_*` 调用清零）。
3. `render.rs` 仅剩 `present` + 图标/图像处理，GDI+ 绘制代码删除。
4. 所有交互行为不变：点击、悬停高亮、托盘闪烁、点击穿透、透明度、四锚位、DPI 1.25、点击回传协议。
5. 视觉一致（唯一允许的差异是符号图标墨迹归一化，需真机确认可接受）。
