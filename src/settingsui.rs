//! 设置页面（egui 版）：一个普通小窗跑 eframe（glow 后端），immediate mode 自绘控件。
//! 所有值都写回 `settings.rs`（= `<exe>/data/settings.txt`），改完立刻生效，
//! 胶囊 / 模块条 / Dock 靠 `settings::version()` 感知变化后重绘。
//!
//! 布局：左 = 分组导航（顶部胶囊/Dock/音乐条/天气/邮箱账户/模块与托盘），
//! 右 = 所选分组的详细设置（组内滚动）。窗口尺寸固定，打开时主屏居中。
//! - 邮箱：内置常用服务器预设（QQ/163/Gmail/Outlook），选完只需填账号 + 授权码，
//!   另有「测试连通」按钮（后台线程 IMAP LOGIN 实测）。
//! - 模块芯片：拖动排序、点击循环 胶囊 → 左条 → 右条 → 隐藏
//! - 托盘图标：两列开关，第 0 格固定是「返回桌面」，下面有「恢复」按钮
//!
//! 线程模型：eframe 线程**懒启动**——首次 `show()` 才起（eframe 渲染完第一帧后
//! 会强制 `set_visible(true)`，启动就建窗会导致设置页开机弹出）。窗口建好后常驻，
//! 之后靠 `QUEUE` + `egui::Context` 的 request_repaint 收发 显示/隐藏 命令；
//! winit 事件循环需显式开 `with_any_thread`（见 `event_loop_builder` 钩子）。
//! 设置页不在任务栏显示（`with_taskbar(false)`）。
//!
//! 打开方式：右键 Dock 空白处、右键胶囊、或 Ctrl+Alt+, （再按一次收起）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui;

use crate::clock;
use crate::config as cfg;
use crate::settings::{self, DockAnchor, LyricPos, ModuleMode, PillAnchor, WeatherSource};

const TITLE: &str = "Orbit 设置";
/// 窗口总宽（固定尺寸，内容超出走组内滚动）
const WIN_W: f32 = 940.0;
const WIN_H: f32 = 560.0;
/// 左侧分组栏宽
const NAV_W: f32 = 150.0;
/// 编辑框/控件宽
const CTRL_W: f32 = 320.0;
/// 窗口圆角
const WIN_RADIUS: f32 = 10.0;

/// 托盘列表里"返回桌面"开关的伪键
const DESKTOP_KEY: &str = "__desktop__";

// ---------- 主线程 ↔ eframe 线程 ----------

enum Cmd {
    Show,
    Hide,
}

static CTX: OnceLock<egui::Context> = OnceLock::new();
static QUEUE: Mutex<VecDeque<Cmd>> = Mutex::new(VecDeque::new());
static OPEN: AtomicBool = AtomicBool::new(false);
/// eframe 线程是否已启动
static SPAWNED: AtomicBool = AtomicBool::new(false);

fn push(cmd: Cmd) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push_back(cmd);
    }
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// 打开 / 收起（热键和右键都走这里）
pub fn toggle() {
    if is_open() {
        hide();
    } else {
        show();
    }
}

pub fn hide() {
    if SPAWNED.load(Ordering::Relaxed) {
        push(Cmd::Hide);
    }
}

pub fn show() {
    // 懒启动：eframe 渲染完第一帧后总会强制 set_visible(true)（epi_integration
    // 的 post_rendering，无视 with_visible(false)），启动时就建窗会让设置页
    // 开机弹出。所以等用户第一次打开设置才起线程；show 命令先进 QUEUE，
    // 线程起来后第一帧 logic 就会消费掉。
    if !SPAWNED.swap(true, Ordering::Relaxed) {
        let _ = std::thread::Builder::new()
            .name("settings-ui".into())
            .spawn(run);
    }
    push(Cmd::Show);
}

/// 设置被别处改了（手改文件）时立刻重画一下（平时 250ms 轮询也能兜底）
pub fn sync() {
    if is_open() {
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
    }
}

/// `init` 现在什么都不做（保留入口兼容旧调用点）：eframe 线程改为首次
/// `show()` 时才启动，避免程序运行后设置页第一时间弹出。
pub fn init(_instance: windows::Win32::Foundation::HINSTANCE) {}

fn run() {
    use winit::platform::windows::EventLoopBuilderExtWindows as _;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_visible(false)
            .with_resizable(false)
            .with_decorations(false)
            // 不置顶：打开时 Focus 弹到最前即可（置顶会挡住别的窗口）
            // Windows：不占任务栏（对齐旧版分层小窗的行为）
            .with_taskbar(false)
            .with_inner_size([WIN_W, WIN_H]),
        // eframe 跑在后台线程：winit 默认禁止非主线程建事件循环，必须显式放开
        event_loop_builder: Some(Box::new(|builder| {
            builder.with_any_thread(true);
        })),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        TITLE,
        options,
        Box::new(|cc| {
            install_fonts(&cc.egui_ctx);
            apply_style(&cc.egui_ctx);
            let _ = CTX.set(cc.egui_ctx.clone());
            Ok(Box::new(SettingsApp::default()))
        }),
    ) {
        crate::tlog!("[settings] eframe 启动失败（设置页不可用）：{e}");
    }
}

// ---------- 外观 ----------

fn argb(argb: u32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        ((argb >> 16) & 0xFF) as u8,
        ((argb >> 8) & 0xFF) as u8,
        (argb & 0xFF) as u8,
        ((argb >> 24) & 0xFF) as u8,
    )
}

/// 把颜色整体调亮/调暗：`k > 1` 提亮、`k < 1` 压暗（alpha 不变）。
/// 用来从面板底色派生输入框底、非交互控件底这些"同一色系的邻近色"。
fn shade(c: u32, k: f32) -> u32 {
    let ch = |v: u32| ((v as f32 * k).round().clamp(0.0, 255.0)) as u32;
    (c & 0xFF00_0000) | (ch((c >> 16) & 0xFF) << 16) | (ch((c >> 8) & 0xFF) << 8) | ch(c & 0xFF)
}

/// 系统 DPI 缩放（物理像素 / 逻辑点）：定位时要用它换算
fn ppp(ctx: &egui::Context) -> f32 {
    ctx.input(|i| i.viewport().native_pixels_per_point)
        .unwrap_or_else(|| ctx.pixels_per_point())
        .max(0.5)
}

/// 中文兜底字体：微软雅黑（没有就黑体），追加到各字体族末尾做回退
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for p in ["C:/Windows/Fonts/msyh.ttc", "C:/Windows/Fonts/msyh.ttf", "C:/Windows/Fonts/simhei.ttf"] {
        if let Ok(bytes) = std::fs::read(p) {
            fonts
                .font_data
                .insert("orbit_cjk".into(), Arc::new(egui::FontData::from_owned(bytes)));
            for fam in fonts.families.values_mut() {
                fam.push("orbit_cjk".into());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

/// 按当前主题配色重建设置页的原生控件样式。
///
/// 深色主题用 egui 的 Dark 视觉打底，浅色主题（云母 / 纸质）用 Light 打底——
/// 只覆盖与 Orbit 自己的胶囊/面板对齐的那几个颜色，其余交给 egui 默认值，
/// 免得浅色下某些控件（滚动条、输入框底）还是黑的。
fn apply_style(ctx: &egui::Context) {
    let t = cfg::cur();
    let th = if t.dark { egui::Theme::Dark } else { egui::Theme::Light };
    let panel = argb(t.panel_bg);
    let text_body = argb(t.text_body);
    let accent = argb(t.accent);
    let hover = argb(t.hover);
    let border = argb(t.panel_border);
    ctx.set_theme(th);
    ctx.style_mut_of(th, |st| {
        let v = &mut st.visuals;
        v.panel_fill = panel;
        v.window_fill = panel;
        v.override_text_color = Some(text_body);
        v.selection.bg_fill = accent;
        v.selection.stroke = egui::Stroke::NONE;
        v.widgets.hovered.bg_fill = hover;
        v.widgets.active.bg_fill = hover;
        v.window_corner_radius = egui::CornerRadius::same(WIN_RADIUS as u8);
        v.window_stroke = egui::Stroke::new(1.0, border);
        if t.dark {
            // 深色下输入框/轨道要比面板再暗一档才有凹陷感；浅色交给 egui 默认
            v.extreme_bg_color = argb(shade(t.panel_bg, 0.86));
            v.widgets.noninteractive.bg_fill = argb(shade(t.panel_bg, 1.14));
        }
    });
}

// ---------- 应用状态 ----------

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Group {
    #[default]
    Pill,
    Dock,
    MusicBar,
    Weather,
    Mail,
    Modules,
}

impl Group {
    const ALL: [Group; 6] = [
        Group::Pill,
        Group::Dock,
        Group::MusicBar,
        Group::Weather,
        Group::Mail,
        Group::Modules,
    ];
    fn label(self) -> &'static str {
        match self {
            Group::Pill => "顶部胶囊",
            Group::Dock => "Dock",
            Group::MusicBar => "音乐条",
            Group::Weather => "天气",
            Group::Mail => "邮箱账户",
            Group::Modules => "模块与托盘",
        }
    }
}

/// 邮箱「测试连通」的状态（后台线程写，UI 线程读）
#[derive(Clone, Default)]
enum TestState {
    #[default]
    Idle,
    Running,
    Ok(String),
    Fail(String),
}

/// 邮箱服务器预设：(显示名, IMAP 主机, 端口)
const MAIL_PRESETS: [(&str, &str, u16); 4] = [
    ("QQ 邮箱", "imap.qq.com", 993),
    ("163 邮箱", "imap.163.com", 993),
    ("Gmail", "imap.gmail.com", 993),
    ("Outlook", "outlook.office365.com", 993),
];

#[derive(Default)]
struct SettingsApp {
    /// 上次见过的设置版本号（值被别处改了要刷新编辑框/开关）
    seen_version: u64,
    /// 上次套用样式的版本号（主题变了要重刷 egui 原生控件配色）
    seen_style_version: u64,
    /// 编辑框的本地缓冲（失焦且版本变化时才从 settings 重读，打字时不被写盘值顶掉）
    edits: Edits,
    /// 模块芯片拖动状态
    drag: Option<Drag>,
    /// 本帧各芯片的矩形（拖动换位用）
    chip_rects: Vec<(String, egui::Rect)>,
    /// 当前选中的分组
    group: Group,
    /// 邮箱测试连通状态（与后台线程共享）
    test: Arc<Mutex<TestState>>,
}

#[derive(Default)]
struct Edits {
    clock: String,
    amap: String,
    host: String,
    port: String,
    user: String,
    pass: String,
}

struct Drag {
    name: String,
    moved: bool,
}

impl eframe::App for SettingsApp {
    /// 窗口隐藏时 eframe 不跑 UI，只调这里：命令收发、关窗拦截、定时唤醒都在这做
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let cmds: Vec<Cmd> = QUEUE
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default();
        for cmd in cmds {
            match cmd {
                Cmd::Show => {
                    self.center(ctx);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    OPEN.store(true, Ordering::Relaxed);
                }
                Cmd::Hide => self.do_hide(ctx),
            }
        }
        // Alt+F4 / 系统关窗：当作收起，不退出 eframe
        if ctx.input(|i| i.viewport().close_requested()) {
            self.do_hide(ctx);
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        // 命令与设置文件改动的轮询兜底（隐藏时也靠它醒）
        let wake = if is_open() { 500 } else { 250 };
        ctx.request_repaint_after(std::time::Duration::from_millis(wake));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ver = settings::version();
        let ver_changed = ver != self.seen_version;
        if self.seen_version == 0 {
            self.load_edits();
        }
        // 主题（或任何设置）变了就重刷原生控件样式；只在变化时做，避免每帧 clone Style
        if ver_changed || self.seen_style_version == 0 {
            apply_style(&ctx);
            self.seen_style_version = ver;
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(argb(cfg::cur().panel_bg))
                    .inner_margin(14.0),
            )
            .show(ui, |ui| {
                self.header(ui, &ctx);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    // 左：分组导航（显式 vertical，防止布局方向从 horizontal 继承）
                    ui.vertical(|ui| {
                        ui.set_width(NAV_W);
                        for g in Group::ALL {
                            let sel = self.group == g;
                            let txt = egui::RichText::new(g.label()).size(13.0);
                            if ui
                                .add_sized([NAV_W, 30.0], egui::Button::selectable(sel, txt))
                                .clicked()
                            {
                                self.group = g;
                            }
                        }
                    });
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(12.0);
                    // 右：所选分组的详细设置（组内滚动；
                    // ScrollArea 会继承父布局方向，内容必须显式 top_down）
                    let detail_w = ui.available_width();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                                ui.set_width(detail_w);
                                self.detail(ui, ver_changed);
                            });
                        });
                });
            });

        self.seen_version = ver;
    }
}

impl SettingsApp {
    fn do_hide(&mut self, ctx: &egui::Context) {
        OPEN.store(false, Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    /// 主屏居中：显示器坐标是物理像素，`OuterPosition` 要逻辑点，按 DPI 换算
    fn center(&self, ctx: &egui::Context) {
        let mon = crate::window::primary_monitor().rcMonitor;
        let k = ppp(ctx);
        let x = (mon.left as f32 + ((mon.right - mon.left) as f32 - WIN_W * k) / 2.0) / k;
        let y = ((mon.top as f32 + ((mon.bottom - mon.top) as f32 - WIN_H * k) / 2.0)
            .max(mon.top as f32 + 6.0))
            / k;
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
    }

    fn load_edits(&mut self) {
        self.edits.clock = settings::clock_format();
        self.edits.amap = settings::amap_key();
        let (h, p, u, w) = settings::mail_overrides();
        self.edits.host = h.unwrap_or_default();
        self.edits.port = p
            .map(|v| v.to_string())
            .unwrap_or_else(|| cfg::MAIL_PORT.to_string());
        self.edits.user = u.unwrap_or_default();
        self.edits.pass = w.unwrap_or_default();
    }

    // ---------- 右侧详情（按分组） ----------

    fn detail(&mut self, ui: &mut egui::Ui, ver_changed: bool) {
        match self.group {
            Group::Pill => self.detail_pill(ui, ver_changed),
            Group::Dock => self.detail_dock(ui),
            Group::MusicBar => self.detail_music(ui),
            Group::Weather => self.detail_weather(ui, ver_changed),
            Group::Mail => self.detail_mail(ui, ver_changed),
            Group::Modules => self.detail_modules(ui),
        }
    }

    fn detail_pill(&mut self, ui: &mut egui::Ui, ver_changed: bool) {
        section(ui, "顶部胶囊");
        egui::Grid::new("g_pill")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label(row_label("时钟格式"));
                let r = ui.add_sized(
                    [CTRL_W, 24.0],
                    egui::TextEdit::singleline(&mut self.edits.clock),
                );
                // 失焦才写盘（每键写文件会卡）；预览直接读本地缓冲，打字时照样实时
                if r.lost_focus() && self.edits.clock != settings::clock_format() {
                    settings::set_clock_format(&self.edits.clock);
                }
                if ver_changed && !r.has_focus() {
                    self.edits.clock = settings::clock_format();
                }
                ui.end_row();

                ui.label(row_label("实时预览"));
                ui.label(
                    egui::RichText::new(clock::sample(&self.edits.clock))
                        .size(13.0)
                        .color(argb(cfg::cur().accent)),
                );
                ui.end_row();

                // 外观主题：一排小色块，直接用主题自己的底色画，所见即所得
                ui.label(row_label("外观"));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    for (i, t) in crate::theme::ALL.iter().enumerate() {
                        let sel = i == settings::theme_index();
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(36.0, 26.0), egui::Sense::click());
                        let p = ui.painter();
                        // 先垫一块"模拟壁纸"：胶囊底都是半透明的，直接画在设置页背景上
                        // 会看不出差别（液态玻璃和深色玻璃会变成同一个灰），垫层中性色才真实
                        p.rect_filled(
                            rect,
                            egui::CornerRadius::same(7),
                            egui::Color32::from_rgb(0x6E, 0x7C, 0x8A),
                        );
                        p.rect_filled(rect, egui::CornerRadius::same(7), argb(t.pill_bg));
                        // 画一条代表主文字的短横线：能直观看出该主题的对比度
                        let bar = egui::Rect::from_center_size(
                            rect.center() - egui::vec2(4.0, 0.0),
                            egui::vec2(13.0, 2.0),
                        );
                        p.rect_filled(bar, 1.0, argb(t.text));
                        p.rect_stroke(
                            rect,
                            egui::CornerRadius::same(7),
                            egui::Stroke::new(1.0, argb(t.dock_edge.max(0x22_00_00_00))),
                            egui::StrokeKind::Inside,
                        );
                        if sel {
                            p.rect_stroke(
                                rect.expand(1.5),
                                egui::CornerRadius::same(9),
                                egui::Stroke::new(1.5, argb(t.accent)),
                                egui::StrokeKind::Outside,
                            );
                        }
                        if resp.clicked() {
                            settings::set_theme_index(i);
                        }
                        resp.on_hover_text(t.name);
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(cfg::cur().name)
                            .size(12.5)
                            .color(argb(cfg::cur().text_dim)),
                    );
                });
                ui.end_row();

                // 透明度：滑条（拖动时只做即时预览，松手才落盘）
                let mut alpha = settings::pill_alpha();
                ui.label(row_label("透明度"));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    // Slider 的宽度取 spacing.slider_width（add_sized 管不到它）
                    ui.spacing_mut().slider_width = CTRL_W - 116.0;
                    let r = ui.add(
                        egui::Slider::new(&mut alpha, settings::ALPHA_MIN..=settings::ALPHA_MAX)
                            .show_value(false)
                            .trailing_fill(true),
                    );
                    if r.changed() {
                        settings::preview_pill_alpha(alpha);
                    }
                    // 松手（或点击轨道、键盘微调这类一次性改动）才写盘
                    if r.drag_stopped() || (r.changed() && !r.dragged()) {
                        settings::persist_pill_alpha();
                    }
                    ui.label(
                        egui::RichText::new(format!("{:.0}%", settings::pill_alpha() * 100.0))
                            .size(12.5)
                            .monospace(),
                    );
                });
                ui.end_row();

                ui.label(row_label("大小"));
                scale_row(
                    ui,
                    settings::pill_scale_setting(),
                    settings::PILL_SCALE_MIN,
                    settings::PILL_SCALE_MAX,
                    settings::preview_pill_scale,
                    settings::persist_pill_scale,
                    || settings::set_pill_scale(1.0),
                );
                ui.end_row();

                ui.label(row_label("形状"));
                let shape = match settings::pill_anchor() {
                    PillAnchor::Float => 0,
                    PillAnchor::Island => 1,
                    PillAnchor::Bottom => 2,
                };
                if let Some(i) = seg(ui, &["悬浮", "吸顶刘海", "吸底"], shape) {
                    settings::set_pill_anchor(match i {
                        1 => PillAnchor::Island,
                        2 => PillAnchor::Bottom,
                        _ => PillAnchor::Float,
                    });
                }
                ui.end_row();
            });
    }

    fn detail_dock(&mut self, ui: &mut egui::Ui) {
        section(ui, "Dock");
        egui::Grid::new("g_dock")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label(row_label("位置"));
                let a = match settings::dock_anchor() {
                    DockAnchor::Bottom => 0,
                    DockAnchor::Top => 1,
                    DockAnchor::Left => 2,
                    DockAnchor::Right => 3,
                };
                if let Some(i) = seg(ui, &["下", "上", "左", "右"], a) {
                    settings::set_dock_anchor(match i {
                        1 => DockAnchor::Top,
                        2 => DockAnchor::Left,
                        3 => DockAnchor::Right,
                        _ => DockAnchor::Bottom,
                    });
                }
                ui.end_row();

                ui.label(row_label("图标大小"));
                scale_row(
                    ui,
                    settings::icon_scale_setting(),
                    settings::ICON_SCALE_MIN,
                    settings::ICON_SCALE_MAX,
                    settings::preview_icon_scale,
                    settings::persist_icon_scale,
                    || settings::set_icon_scale(1.0),
                );
                ui.end_row();
            });
        ui.label(
            egui::RichText::new("图标大小会连同 Dock 高度、槽位和托盘图标等比缩放；点「重置」回到默认尺寸。")
                .size(11.5)
                .weak(),
        );

        ui.add_space(10.0);
        self.pins(ui);
    }

    /// 固定图标：已固定的一行一个（右侧「移除」），下面列正在运行但还没固定的应用，
    /// 再给一个「浏览程序…」从磁盘挑 exe / 快捷方式。所有改动都整体写回列表。
    fn pins(&mut self, ui: &mut egui::Ui) {
        section(ui, "固定图标");
        let (pins, running) = pins_view();

        if pins.is_empty() {
            ui.label(
                egui::RichText::new("还没有固定图标：只显示正在运行的应用")
                    .size(11.5)
                    .weak(),
            );
        }
        for path in &pins {
            let label = pin_label(path);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(egui::RichText::new(label).size(12.5));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("移除").small()).clicked() {
                        let mut list = pins.clone();
                        list.retain(|p| !p.eq_ignore_ascii_case(path));
                        settings::set_dock_pins(list);
                    }
                });
            });
        }

        ui.add_space(6.0);
        ui.label(egui::RichText::new("添加：正在运行的应用").size(11.5).weak());
        let running: Vec<(String, String)> = running
            .into_iter()
            .filter(|(_, exe)| !pins.iter().any(|p| p.eq_ignore_ascii_case(exe)))
            .collect();
        if running.is_empty() {
            ui.label(egui::RichText::new("（没有可添加的运行中应用）").size(11.5).weak());
        }
        for (name, exe) in &running {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(egui::RichText::new(name).size(12.5));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("固定").small()).clicked() {
                        let mut list = pins.clone();
                        list.push(exe.clone());
                        settings::set_dock_pins(list);
                    }
                });
            });
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new("浏览程序…").small()).clicked() {
                if let Some(path) = pick_program() {
                    let mut list = pins.clone();
                    if !list.iter().any(|p| p.eq_ignore_ascii_case(&path)) {
                        list.push(path);
                        settings::set_dock_pins(list);
                    }
                }
            }
            if !pins.is_empty() && ui.add(egui::Button::new("清空固定").small()).clicked() {
                settings::set_dock_pins(Vec::new());
            }
        });
    }

    fn detail_music(&mut self, ui: &mut egui::Ui) {
        section(ui, "音乐条");
        egui::Grid::new("g_lyric")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label(row_label("位置"));
                let p = match settings::lyric_position() {
                    LyricPos::Left => 0,
                    LyricPos::Right => 1,
                    LyricPos::Bottom => 2,
                };
                if let Some(i) = seg(ui, &["左", "右", "下"], p) {
                    settings::set_lyric_position(match i {
                        0 => LyricPos::Left,
                        2 => LyricPos::Bottom,
                        _ => LyricPos::Right,
                    });
                }
                ui.end_row();

                let size = settings::lyric_size();
                ui.label(row_label("歌词字号"));
                if let Some(d) = stepper(ui, &format!("{size:.0}"), size > 10.0, size < 26.0) {
                    settings::set_lyric_size(size + d as f32);
                }
                ui.end_row();
            });
    }

    fn detail_weather(&mut self, ui: &mut egui::Ui, ver_changed: bool) {
        section(ui, "天气");
        egui::Grid::new("g_weather")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                let s = settings::weather_text_scale();
                ui.label(row_label("字号倍率"));
                if let Some(d) = stepper(ui, &format!("{s:.2}×"), s > 0.8 + 1e-3, s < 1.8 - 1e-3) {
                    settings::set_weather_text_scale(s + d as f32 * 0.05);
                }
                ui.end_row();

                ui.label(row_label("数据源"));
                let ws = match settings::weather_source() {
                    WeatherSource::Amap => 0,
                    WeatherSource::OpenMeteo => 1,
                };
                if let Some(i) = seg(ui, &["高德", "Open-Meteo"], ws) {
                    settings::set_weather_source(match i {
                        1 => WeatherSource::OpenMeteo,
                        _ => WeatherSource::Amap,
                    });
                }
                ui.end_row();

                ui.label(row_label("高德 Key"));
                let r = ui.add_sized(
                    [CTRL_W, 24.0],
                    egui::TextEdit::singleline(&mut self.edits.amap),
                );
                if r.lost_focus() && self.edits.amap != settings::amap_key() {
                    settings::set_amap_key(&self.edits.amap);
                }
                if ver_changed && !r.has_focus() {
                    self.edits.amap = settings::amap_key();
                }
                ui.end_row();
            });
    }

    fn detail_mail(&mut self, ui: &mut egui::Ui, ver_changed: bool) {
        section(ui, "服务器");
        // 当前 host 命中预设就不显示 host/port 编辑框，只需填账号
        let custom = !MAIL_PRESETS.iter().any(|(_, h, _)| *h == self.edits.host);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (name, host, port) in MAIL_PRESETS {
                let sel = self.edits.host == host;
                if ui
                    .add(egui::Button::selectable(
                        sel,
                        egui::RichText::new(name).size(12.5),
                    ))
                    .clicked()
                {
                    self.edits.host = host.to_string();
                    self.edits.port = port.to_string();
                    self.commit_mail();
                }
            }
            if ui
                .add(egui::Button::selectable(
                    custom,
                    egui::RichText::new("自定义").size(12.5),
                ))
                .clicked()
            {
                // 切到"自定义"只展开编辑框，不改当前值
                if !custom {
                    self.edits.host.clear();
                    self.edits.port = cfg::MAIL_PORT.to_string();
                    self.commit_mail();
                }
            }
        });

        if custom {
            egui::Grid::new("g_mail_srv")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label(row_label("服务器"));
                    let r = ui.add_sized(
                        [CTRL_W, 24.0],
                        egui::TextEdit::singleline(&mut self.edits.host).hint_text("imap.example.com"),
                    );
                    if r.lost_focus() {
                        self.commit_mail();
                    }
                    ui.end_row();

                    ui.label(row_label("端口"));
                    let r = ui.add_sized(
                        [CTRL_W, 24.0],
                        egui::TextEdit::singleline(&mut self.edits.port),
                    );
                    if r.lost_focus() {
                        self.commit_mail();
                    }
                    ui.end_row();
                });
        } else {
            ui.label(
                egui::RichText::new(format!(
                    "{}:{}（IMAPS，SSL 加密）",
                    self.edits.host, self.edits.port
                ))
                .size(12.0)
                .weak(),
            );
        }

        ui.add_space(4.0);
        section(ui, "账户");
        egui::Grid::new("g_mail_acc")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label(row_label("账号"));
                let r = ui.add_sized(
                    [CTRL_W, 24.0],
                    egui::TextEdit::singleline(&mut self.edits.user),
                );
                if r.lost_focus() {
                    self.commit_mail();
                }
                if ver_changed && !r.has_focus() {
                    self.edits.user = settings::mail_overrides().2.unwrap_or_default();
                }
                ui.end_row();

                ui.label(row_label("授权码"));
                let r = ui.add_sized(
                    [CTRL_W, 24.0],
                    egui::TextEdit::singleline(&mut self.edits.pass).password(true),
                );
                if r.lost_focus() {
                    self.commit_mail();
                }
                if ver_changed && !r.has_focus() {
                    self.edits.pass = settings::mail_overrides().3.unwrap_or_default();
                }
                ui.end_row();
            });

        ui.add_space(8.0);
        let port: u16 = self.edits.port.trim().parse().unwrap_or(cfg::MAIL_PORT);
        let st = self
            .test
            .lock()
            .map(|s| s.clone())
            .unwrap_or(TestState::Idle);
        ui.horizontal(|ui| {
            let running = matches!(st, TestState::Running);
            if ui.add_enabled(!running, egui::Button::new("测试连通")).clicked() {
                let (h, u, w) =
                    (self.edits.host.clone(), self.edits.user.clone(), self.edits.pass.clone());
                if h.is_empty() || u.is_empty() || w.is_empty() {
                    if let Ok(mut s) = self.test.lock() {
                        *s = TestState::Fail("请先填写服务器、账号和授权码".into());
                    }
                } else {
                    if let Ok(mut s) = self.test.lock() {
                        *s = TestState::Running;
                    }
                    let shared = self.test.clone();
                    // IMAP 登录是秒级网络 I/O，放后台线程，UI 只轮询结果
                    std::thread::spawn(move || {
                        let res = crate::mail::test_account(&(h, port, u, w))
                            .map(|_| "连接成功，账号密码有效".to_string());
                        if let Ok(mut s) = shared.lock() {
                            *s = match res {
                                Ok(msg) => TestState::Ok(msg),
                                Err(e) => TestState::Fail(e),
                            };
                        }
                    });
                }
            }
            match &st {
                TestState::Idle => {}
                TestState::Running => {
                    ui.label(egui::RichText::new("测试中…").size(12.5).weak());
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(300));
                }
                TestState::Ok(msg) => {
                    ui.label(egui::RichText::new(format!("✓ {msg}")).size(12.5).color(egui::Color32::from_rgb(0x6E, 0xC2, 0x7A)));
                }
                TestState::Fail(msg) => {
                    ui.label(egui::RichText::new(format!("✗ {msg}")).size(12.5).color(egui::Color32::from_rgb(0xE0, 0x6C, 0x6C)));
                }
            }
        });
        ui.label(
            egui::RichText::new("提示：QQ/163 等邮箱需在网页版邮箱设置里开启 IMAP，并用「授权码」而非登录密码。")
                .size(11.5)
                .weak(),
        );
    }

    fn detail_modules(&mut self, ui: &mut egui::Ui) {
        section(ui, "顶部胶囊模块");
        self.chips(ui);
        ui.label(
            egui::RichText::new("点击芯片切换：胶囊 → 左条 → 右条 → 隐藏；拖动排序")
                .size(11.5)
                .weak(),
        );

        ui.add_space(10.0);
        section(ui, "托盘图标");
        let mut entries: Vec<(String, String)> = vec![(DESKTOP_KEY.to_string(), "返回桌面".into())];
        entries.extend(tray_entries());
        let cell_w = (ui.available_width() - 10.0).min(300.0) / 2.0;
        for chunk in entries.chunks(2) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                for (key, name) in chunk {
                    let on = if key == DESKTOP_KEY {
                        settings::show_desktop_btn()
                    } else {
                        !settings::tray_hidden(key)
                    };
                    let fill = if on {
                        argb(0x66_3C_8D_FF)
                    } else {
                        argb(0x33_FF_FF_FF)
                    };
                    let txt =
                        egui::RichText::new(name.as_str()).size(12.0).color(if on {
                            egui::Color32::WHITE
                        } else {
                            argb(0x8C_FF_FF_FF)
                        });
                    if ui
                        .add(egui::Button::new(txt).fill(fill).min_size(egui::vec2(cell_w, 24.0)))
                        .clicked()
                    {
                        if key == DESKTOP_KEY {
                            settings::set_show_desktop_btn(!settings::show_desktop_btn());
                            crate::tlog!(
                                "[settings] 返回桌面细长条 → {}",
                                if settings::show_desktop_btn() { "显示" } else { "隐藏" }
                            );
                        } else {
                            let now_hidden = !settings::tray_hidden(key);
                            settings::set_tray_hidden(key, now_hidden);
                            crate::tlog!(
                                "[settings] 托盘图标 {key} → {}",
                                if now_hidden { "隐藏" } else { "显示" }
                            );
                        }
                    }
                }
            });
        }
        if !settings::hidden_tray().is_empty() {
            ui.add_space(6.0);
            if ui.add(egui::Button::new("恢复全部显示").small()).clicked() {
                settings::clear_hidden_tray();
                crate::tlog!("[settings] 托盘隐藏列表清空");
            }
        }
    }

    /// 邮箱字段：任一变化都按"当前值组合"整体提交（对齐旧版 commit_edit）
    fn commit_mail(&mut self) {
        let port: u16 = self.edits.port.trim().parse().unwrap_or(cfg::MAIL_PORT);
        settings::set_mail_account(
            &self.edits.host.clone(),
            port,
            &self.edits.user.clone(),
            &self.edits.pass.clone(),
        );
    }

    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Orbit 设置").size(16.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if close_button(ui).clicked() {
                    self.do_hide(ctx);
                }
            });
        });
        ui.add_space(4.0);
        ui.separator();
    }

    /// 模块芯片：拖动排序（扫过别的芯片中部就换位）+ 点击循环状态
    fn chips(&mut self, ui: &mut egui::Ui) {
        self.chip_rects.clear();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            // 固定芯片：时间日期永远在最前
            ui.add(
                egui::Button::new(egui::RichText::new("时间 · 日期").size(12.0))
                    .fill(argb(0x33_FF_FF_FF)),
            );
            for name in settings::module_order() {
                let m = settings::module_mode(&name);
                let tag = match m {
                    ModuleMode::Pill => "胶囊",
                    ModuleMode::BarLeft => "左条",
                    ModuleMode::BarRight => "右条",
                    ModuleMode::Off => "隐藏",
                };
                let fill = match m {
                    ModuleMode::Pill => argb(0x66_3C_8D_FF),
                    ModuleMode::BarLeft => argb(0x5C_2E_8F_6E),
                    ModuleMode::BarRight => argb(0x5C_8A_5A_C8),
                    ModuleMode::Off => argb(0x2E_FF_FF_FF),
                };
                let label = format!("{} · {tag}", module_label(&name));
                let r = ui.add(
                    egui::Button::new(egui::RichText::new(label).size(12.0)).fill(fill),
                );
                self.chip_rects.push((name.clone(), r.rect));
                if r.drag_started() {
                    self.drag = Some(Drag { name: name.clone(), moved: false });
                }
                if let Some(d) = &mut self.drag {
                    if d.name == name && r.dragged() {
                        let moved = ui.input(|i| {
                            let d = i.pointer.delta();
                            d.x.abs() + d.y.abs() > 3.0
                        });
                        if moved {
                            d.moved = true;
                        }
                    }
                }
                if r.clicked() {
                    let next = match m {
                        ModuleMode::Pill => ModuleMode::BarLeft,
                        ModuleMode::BarLeft => ModuleMode::BarRight,
                        ModuleMode::BarRight => ModuleMode::Off,
                        ModuleMode::Off => ModuleMode::Pill,
                    };
                    settings::set_module_mode(&name, next);
                    crate::tlog!("[settings] 模块 {name} → {next:?}");
                }
            }
        });

        // 拖动换位：指针落在别的芯片中部就交换顺序（写 settings，立即生效）
        if let Some(d) = &self.drag {
            if d.moved {
                if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                    for (other, rect) in &self.chip_rects {
                        if *other == d.name || !rect.contains(p) {
                            continue;
                        }
                        // 只认芯片中部（带滞回，防止两枚芯片来回打架）
                        if (p.x - rect.center().x).abs() < rect.width() * 0.3 {
                            let mut order = settings::module_order();
                            if let (Some(a), Some(b)) = (
                                order.iter().position(|n| *n == d.name),
                                order.iter().position(|n| n == other),
                            ) {
                                order.swap(a, b);
                                settings::set_module_order(order);
                                crate::tlog!("[settings] 模块顺序 → {:?}", settings::module_order());
                            }
                            break;
                        }
                    }
                }
            }
        }
        if ui.input(|i| i.pointer.any_released()) {
            self.drag = None;
        }
    }
}

// ---------- 小部件 ----------

/// 标题栏关闭按钮：**自绘**两条斜线组成 ✕。
///
/// 不能用文字 `✕`（U+2715）：本机字体（雅黑 / 黑体）没有这个字形，
/// 未启用 default_fonts 时也没有 emoji 兜底，会渲染成一个空白豆腐块。
fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 24.0), egui::Sense::click());
    let hovered = resp.hovered();
    let p = ui.painter();
    if hovered {
        p.rect_filled(rect, 6.0_f32, argb(cfg::cur().hover));
    }
    let c = rect.center();
    let s = 4.5;
    // 常态用次级文字色、悬停升到主文字色：浅色主题下白线会消失在白底里
    let stroke = egui::Stroke::new(
        1.5,
        argb(if hovered { cfg::cur().text } else { cfg::cur().text_dim }),
    );
    p.line_segment([c + egui::vec2(-s, -s), c + egui::vec2(s, s)], stroke);
    p.line_segment([c + egui::vec2(s, -s), c + egui::vec2(-s, s)], stroke);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(title).size(11.5).weak());
    ui.add_space(2.0);
}

fn row_label(s: &str) -> egui::RichText {
    egui::RichText::new(s).size(13.0)
}

/// 分段选择：返回被点中的段（None = 没点）
fn seg(ui: &mut egui::Ui, options: &[&str], active: usize) -> Option<usize> {
    let mut pick = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, o) in options.iter().enumerate() {
            let btn = egui::Button::selectable(i == active, egui::RichText::new(*o).size(12.0));
            if ui.add(btn).clicked() {
                pick = Some(i);
            }
        }
    });
    pick
}

/// 尺寸滑条 + 百分比 + 「重置」。
///
/// 拖动过程只做即时预览（`preview`），松手才落盘（`persist`）——和透明度滑条
/// 同一套逻辑，避免每移一像素就重写一次 settings.txt。
/// 「固定图标」区块的输入数据：(已固定列表, 正在运行的应用)。
///
/// immediate mode 每帧都会重跑界面函数，`EnumWindows` + 读 TaskBar 目录太贵，
/// 这里按 2 秒缓存一份；固定列表本身改动会立刻体现在下一帧（缓存被 `set_dock_pins` 之外的
/// 路径不会清，所以保留 2 秒的自然过期）。
fn pins_view() -> (Vec<String>, Vec<(String, String)>) {
    static CACHE: Mutex<Option<(std::time::Instant, Vec<String>, Vec<(String, String)>)>> =
        Mutex::new(None);
    if let Ok(c) = CACHE.lock() {
        if let Some((at, pins, running)) = c.as_ref() {
            if at.elapsed().as_millis() < 2000 {
                return (pins.clone(), running.clone());
            }
        }
    }
    let pins = crate::dock::effective_pins();
    let running = crate::dock::running_app_list();
    if let Ok(mut c) = CACHE.lock() {
        *c = Some((std::time::Instant::now(), pins.clone(), running.clone()));
    }
    (pins, running)
}

/// 固定列表里显示的名字：去掉目录和扩展名
fn pin_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string())
}

/// 挑一个 exe / 快捷方式（系统「打开文件」对话框）
fn pick_program() -> Option<String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OPENFILENAMEW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST,
    };
    let mut buf = vec![0u16; 1024];
    let filter: Vec<u16> = "程序 (*.exe;*.lnk)\0*.exe;*.lnk\0所有文件 (*.*)\0*.*\0\0"
        .encode_utf16()
        .collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_EXPLORER,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
        return None;
    }
    let s = String::from_utf16_lossy(&buf);
    let s = s.trim_end_matches('\0').trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn scale_row(
    ui: &mut egui::Ui,
    value: f32,
    min: f32,
    max: f32,
    preview: fn(f32),
    persist: fn(),
    reset: impl Fn(),
) {
    let mut v = value;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        // Slider 宽度取 spacing.slider_width（add_sized 管不到它）
        ui.spacing_mut().slider_width = CTRL_W - 148.0;
        let r = ui.add(
            egui::Slider::new(&mut v, min..=max)
                .show_value(false)
                .trailing_fill(true),
        );
        if r.changed() {
            preview(v);
        }
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            persist();
        }
        ui.label(
            egui::RichText::new(format!("{:.0}%", v * 100.0))
                .size(12.5)
                .monospace(),
        );
        // 默认 100% 时按钮没意义，置灰
        let at_default = (v - 1.0).abs() < 1e-3;
        if ui
            .add_enabled(!at_default, egui::Button::new("重置").small())
            .on_hover_text("回到默认大小（100%）")
            .clicked()
        {
            reset();
        }
    });
}

/// 加减器：返回 -1 / +1（None = 没点）
fn stepper(ui: &mut egui::Ui, value: &str, can_dec: bool, can_inc: bool) -> Option<i8> {
    let mut out = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let btn = |t: &str| {
            egui::Button::new(egui::RichText::new(t).size(12.0)).min_size(egui::vec2(24.0, 22.0))
        };
        if ui.add_enabled(can_dec, btn("−")).clicked() {
            out = Some(-1);
        }
        ui.add_sized(
            [56.0, 22.0],
            egui::Label::new(egui::RichText::new(value).size(12.5)).halign(egui::Align::Center),
        );
        if ui.add_enabled(can_inc, btn("+")).clicked() {
            out = Some(1);
        }
    });
    out
}

fn module_label(name: &str) -> &'static str {
    match name {
        "weather" => "天气",
        "mail" => "邮箱",
        _ => "剪贴板",
    }
}

// ---------- 托盘列表 ----------

/// 当前注册着的托盘图标（(稳定键, 显示名)），顺序与 Dock 上一致。
fn tray_entries() -> Vec<(String, String)> {
    use crate::tray::icon_key;
    let Some(tab) = crate::trayhost::table() else { return Vec::new() };
    let icons = match tab.lock() {
        Ok(st) => st.icons.clone(),
        Err(_) => return Vec::new(),
    };
    icons
        .iter()
        .map(|ic| {
            let mut name = ic.tip.lines().next().unwrap_or("").trim().to_string();
            if name.is_empty() {
                name = std::path::Path::new(&ic.exe)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
            }
            if name.is_empty() {
                name = "（无名称图标）".to_string();
            }
            (icon_key(Some(ic), &ic.exe, &ic.tip), short(&name, 10))
        })
        .collect()
}

/// 名字太长就截断（格子窄，超出会挤掉右侧开关）
fn short(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}
