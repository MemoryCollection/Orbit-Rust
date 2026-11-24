//! 设置页面（egui 版）：一个普通小窗跑 eframe（glow 后端），immediate mode 自绘控件。
//! 所有值都写回 `settings.rs`（= `<exe>/data/settings.txt`），改完立刻生效，
//! 胶囊 / 模块条 / Dock 靠 `settings::version()` 感知变化后重绘。
//!
//! 布局：左 = 分组导航（常规 / 顶部胶囊 / 通知 / 功能模块 / Dock / 开始菜单 / 微信），
//! 右 = 所选分组的详细设置（组内滚动）。窗口尺寸固定，打开时主屏居中。
//! - 邮箱：多账号管理，内置常用服务器预设（QQ/163/Gmail/Outlook），
//!   「测试连通」通过后打 ✓ 标记并持久化
//! - 模块芯片：拖动排序、点击循环 胶囊 → 左条 → 右条 → 隐藏
//! - 托盘图标：统一列表（一行完成 显示/隐藏 + 图标指定 + ▲▼ 排序），「返回桌面」固定第一行
//! - 开始菜单：Win 键接管、每行列数、快捷方式增删排序（拖动行首圆点或 ↑↓，「快速添加」一键加任务管理器）
//!
//! 线程模型：设置页跑在**独立子进程**（`orbit.exe --settings-ui`）里——
//! winit 不允许同一进程重建事件循环，而"关闭即回收内存"必须让整个 eframe
//! 退出，所以每次打开都起一个新进程，关窗（或主进程发 WM_CLOSE）就进程退出，
//! GL 上下文 / 字体图集 / UI 内存全部归还。
//!
//! 托盘图标表和微信 UIA 实例只活在主进程里，所以主进程在设置页打开期间把这两项
//! 写进 `data/settings-runtime.txt`（`publish_runtime`），子进程读快照；
//! 「应用」「手动拿 UIA」这类动作由子进程 PostMessage 到主进程胶囊窗口完成。
//!
//! 设置页是一个**普通窗口**（`with_taskbar(true)`）：出现在 Alt+Tab 与 Dock 的
//! 运行中应用里，点任务栏图标即可把它拉回前台；没有系统边框，标题栏自己承担
//! 拖动（`ViewportCommand::StartDrag`）。
//!
//! 打开方式：右键 Dock 空白处、右键胶囊、或 Ctrl+Alt+, （再按一次收起）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::clock;
use crate::config as cfg;
use crate::egui_soft;
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

// ---------- 主进程 ↔ 设置页子进程 ----------
//
// winit 不允许在同一个进程里重建事件循环（"EventLoop can't be recreated"），
// 所以设置页跑在**独立子进程**里（`orbit.exe --settings-ui`）：
// - 打开 = 起一个子进程；关闭 = 子进程整个退出，GL 上下文 / 字体图集 / UI 内存
//   全部归还，内存曲线真正归零；
// - 再打开就是全新进程，不存在事件循环复用问题。

/// 设置页子进程句柄（None = 没开 / 已退出）
static CHILD: Mutex<Option<std::process::Child>> = Mutex::new(None);
/// 子进程内：已请求关闭视口（这次 close 放行，不再重复 do_hide）
static CLOSE_REQ: AtomicBool = AtomicBool::new(false);
/// 本进程是不是设置页子进程
static IS_CHILD: AtomicBool = AtomicBool::new(false);
/// 子进程从主进程拿到的运行期快照（托盘图标 / 微信实例），只在子进程里用
static RUNTIME: Mutex<Option<RuntimeSnap>> = Mutex::new(None);

/// 主进程写给子进程的运行期快照。
/// 托盘图标表和微信 UIA 实例都活在主进程里，设置页换成子进程后拿不到内存数据，
/// 所以主进程在设置页打开期间把这两项写进 `data/settings-runtime.txt`，子进程读。
#[derive(Default, Clone)]
struct RuntimeSnap {
    /// (icon_key, 显示名)
    tray: Vec<(String, String)>,
    /// (微信 pid, 该 pid 的 UIA 是否可读)——账号监控表用
    wechat_pid: Vec<(u32, bool)>,
    /// (微信 pid, 绑定热键)——账号监控表用
    wechat_hk: Vec<(u32, String)>,
}

fn runtime_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("data").join("settings-runtime.txt")))
        .unwrap_or_else(|| std::path::PathBuf::from("settings-runtime.txt"))
}

/// 主进程调用：把托盘图标 / 微信实例状态写给设置页子进程。
/// 内容没变就不写盘（每秒调用一次也不会有 I/O 压⼒）。
pub fn publish_runtime() {
    if IS_CHILD.load(Ordering::Relaxed) {
        return;
    }
    let mut tray: Vec<(String, String)> = Vec::new();
    if let Some(tab) = crate::trayhost::table() {
        if let Ok(st) = tab.lock() {
            for ic in st.icons.iter() {
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
                tray.push((crate::tray::icon_key(Some(ic), &ic.exe, &ic.tip), short(&name, 10)));
            }
        }
    }
    let mut body = String::new();
    for (k, n) in &tray {
        body.push_str(&format!("T\t{k}\t{n}\n"));
    }
    // 微信 pid 维度的 UIA 可读状态 + 热键：设置页「账号监控」表格用
    for (pid, got) in crate::wechatmsg::grab_status() {
        body.push_str(&format!("G\t{pid}\t{}\n", if got { 1 } else { 0 }));
    }
    for (pid, hk) in crate::wechatmsg::pid_hotkeys() {
        body.push_str(&format!("H\t{pid}\t{hk}\n"));
    }
    let path = runtime_path();
    if let Ok(old) = std::fs::read_to_string(&path) {
        if old == body {
            return;
        }
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, body);
}

/// 子进程调用：读主进程写的运行期快照
fn load_runtime() {
    let mut snap = RuntimeSnap::default();
    if let Ok(text) = std::fs::read_to_string(runtime_path()) {
        for line in text.lines() {
            let mut it = line.split('\t');
            match (it.next(), it.next(), it.next()) {
                (Some("T"), Some(k), Some(n)) => snap.tray.push((k.to_string(), n.to_string())),
                (Some("G"), Some(pid), Some(got)) => {
                    if let Ok(p) = pid.trim().parse::<u32>() {
                        snap.wechat_pid.push((p, got.trim() == "1"));
                    }
                }
                (Some("H"), Some(pid), Some(hk)) => {
                    if let Ok(p) = pid.trim().parse::<u32>() {
                        snap.wechat_hk.push((p, hk.to_string()));
                    }
                }
                _ => {}
            }
        }
    }
    if let Ok(mut g) = RUNTIME.lock() {
        *g = Some(snap);
    }
}

/// 子进程里读快照；主进程直接返回 None（走内存表）
fn runtime_snap() -> Option<RuntimeSnap> {
    if !IS_CHILD.load(Ordering::Relaxed) {
        return None;
    }
    RUNTIME.lock().ok().and_then(|g| g.clone())
}

/// 该微信 pid 的 UIA 是否可读：子进程用主进程快照，主进程读内存
fn wechat_pid_readable(pid: u32) -> bool {
    match runtime_snap() {
        Some(s) => s.wechat_pid.iter().any(|(p, g)| *p == pid && *g),
        None => crate::wechatmsg::pid_readable(pid),
    }
}

/// 该微信 pid 绑定的热键：子进程用主进程快照，主进程读内存；空串 = 未配置
fn wechat_pid_hotkey(pid: u32) -> String {
    match runtime_snap() {
        Some(s) => s
            .wechat_hk
            .iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, hk)| hk.clone())
            .unwrap_or_default(),
        None => crate::wechatmsg::pid_hotkeys()
            .into_iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, hk)| hk)
            .unwrap_or_default(),
    }
}

/// 找主进程里某个类名的窗口（类名见 app.rs / dock.rs / weathercard.rs 的注册）
fn find_main_window(class: windows::core::PCWSTR) -> Option<windows::Win32::Foundation::HWND> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    unsafe {
        match FindWindowW(class, PCWSTR::null()) {
            Ok(h) if !h.is_invalid() => Some(h),
            _ => None,
        }
    }
}

/// 给主进程的所有窗口投一条消息：胶囊 + Dock + 天气卡片。
///
/// 早先只投胶囊，Dock 得等自己的 500ms 轮询才更新，改 Dock 透明度/图标大小时
/// 会明显"慢半拍"。跨进程拿不到 appmsg 里登记的 HWND，只能按类名找。
fn post_to_parent(msg: u32) {
    post_to_parent_w(msg, 0);
}

/// 同 [`post_to_parent`]，带 wParam（如微信 pid）
fn post_to_parent_w(msg: u32, wparam: usize) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
    for class in [
        windows::core::w!("ORBIT_CAPSULE"),
        windows::core::w!("ORBIT_DOCK"),
        windows::core::w!("ORBIT_WEATHER_CARD"),
    ] {
        if let Some(h) = find_main_window(class) {
            unsafe {
                let _ = PostMessageW(h, msg, WPARAM(wparam), LPARAM(0));
            }
        }
    }
}

pub fn is_open() -> bool {
    let Ok(mut g) = CHILD.lock() else {
        return false;
    };
    let Some(c) = g.as_mut() else {
        return false;
    };
    // 子进程已退出（用户关掉窗口）就顺手清掉句柄
    match c.try_wait() {
        Ok(Some(_)) => {
            *g = None;
            false
        }
        Ok(None) => true,
        Err(_) => {
            *g = None;
            false
        }
    }
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
    let pid = {
        let Ok(mut g) = CHILD.lock() else { return };
        let Some(c) = g.as_mut() else { return };
        match c.try_wait() {
            Ok(Some(_)) => {
                *g = None;
                return;
            }
            _ => c.id(),
        }
    };
    if !post_close(pid) {
        // 窗口还没建出来（刚点开就关）：直接收掉进程，没什么可保存的
        if let Ok(mut g) = CHILD.lock() {
            if let Some(mut c) = g.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
    crate::tlog!("[settings] 已请求关闭设置页（pid={pid}）");
}

pub fn show() {
    if is_open() {
        return;
    }
    // 托盘图标 / 微信实例状态只有主进程有，先把快照写出去给子进程读
    publish_runtime();
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            crate::tlog!("[settings] 取 exe 路径失败：{e}");
            return;
        }
    };
    match std::process::Command::new(exe).arg("--settings-ui").spawn() {
        Ok(c) => {
            crate::tlog!("[settings] 设置页子进程已启动 pid={}", c.id());
            if let Ok(mut g) = CHILD.lock() {
                *g = Some(c);
            }
        }
        Err(e) => crate::tlog!("[settings] 设置页子进程启动失败：{e}"),
    }
}

/// 设置被别处改了（手改文件）：设置页是独立进程，自己 500ms 轮询重绘，这里不用管
pub fn sync() {}

/// 主进程退出前调用：别把设置页窗口留成孤儿
pub fn shutdown() {
    if let Ok(mut g) = CHILD.lock() {
        if let Some(mut c) = g.take() {
            let _ = c.kill();
            let _ = c.wait();
            crate::tlog!("[settings] 退出时已结束设置页子进程");
        }
    }
}

/// 给指定 pid 的顶层窗口发 WM_CLOSE（让设置页走正常的"丢弃改动 + 退出"流程）。
/// 返回 false 表示没找到窗口。
fn post_close(pid: u32) -> bool {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
    };

    struct Ctx {
        pid: u32,
        hwnd: Option<HWND>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, l: LPARAM) -> BOOL {
        let c = &mut *(l.0 as *mut Ctx);
        let mut owner = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut owner));
        if owner == c.pid && IsWindowVisible(hwnd).as_bool() {
            c.hwnd = Some(hwnd);
            return false.into(); // 找到就停
        }
        true.into()
    }

    let mut c = Ctx { pid, hwnd: None };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut c as *mut Ctx as isize));
        if let Some(h) = c.hwnd {
            return PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok();
        }
    }
    false
}

/// `init` 保留入口兼容旧调用点：设置页改为子进程，主进程侧不需要预建任何东西
pub fn init(_instance: windows::Win32::Foundation::HINSTANCE) {}

/// 设置页子进程入口：在本进程里跑 eframe，关窗即进程退出
pub fn run_child() {
    use winit::platform::windows::EventLoopBuilderExtWindows as _;
    IS_CHILD.store(true, Ordering::Relaxed);
    // 子进程是独立进程，settings 的静态变量全是 UNLOADED——必须先读一次本地
    // settings.txt，否则设置页显示的是编译期默认值，点「应用」还会把默认值写回去，
    // 直接覆盖掉用户的配置。（同进程时代这条由主进程的 window.rs 负责，改子进程后
    // 落到这里。）
    crate::settings::init();
    crate::tlog!(
        "[settings] 子进程已载入本地配置：version={} theme={} alpha={:.2}",
        crate::settings::version(),
        crate::settings::theme_index(),
        crate::settings::ui_alpha()
    );
    load_runtime();
    let options = eframe::NativeOptions {
        // 建窗前就算好居中位置（默认 false：窗口先出现在左上角，首帧再跳到中间，
        // 看起来会"闪一下"）。eframe 会在 winit 建窗前用 with_position 落位。
        centered: true,
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_visible(false)
            .with_resizable(false)
            // 还是自绘边框（圆角 + 主题色一体），但窗口要能被拖动，
            // 拖动由标题栏自己发 `StartDrag` 实现（见 `header`）
            .with_decorations(false)
            // 不置顶：打开时 Focus 弹到最前即可（置顶会挡住别的窗口）
            // Windows：占任务栏 = 出现在 Alt+Tab 和 Dock 的"运行中应用"里，
            // 点任务栏图标就能把它拉回前台（不占任务栏时是 WS_EX_TOOLWINDOW，
            // 两边都会被系统过滤掉）
            .with_taskbar(true)
            .with_inner_size([WIN_W, WIN_H]),
        // 允许在非主线程建事件循环（保留原行为，子进程里其实是主线程）
        event_loop_builder: Some(Box::new(|builder| {
            builder.with_any_thread(true);
        })),
        ..Default::default()
    };
    CLOSE_REQ.store(false, Ordering::Relaxed);
    if let Err(e) = eframe::run_native(
        TITLE,
        options,
        Box::new(|cc| {
            install_fonts(&cc.egui_ctx);
            apply_style(&cc.egui_ctx);
            Ok(Box::new(SettingsApp::default()))
        }),
    ) {
        crate::tlog!("[settings] eframe 启动失败（设置页不可用）：{e}");
    }
    crate::tlog!("[settings] 设置页子进程退出");
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
/// 强调色填充上该用的文字色：按填充亮度选深底白字 / 浅底深字。
///
/// M3 深色的主色是浅紫（`#C8BEF0`），压在它上面的白字几乎读不出来；统一按亮度
/// 反选就能同时照顾浅色主题（深强调色 → 白字）。
fn ink_on(fill: u32) -> egui::Color32 {
    let r = ((fill >> 16) & 0xFF) as f32;
    let g = ((fill >> 8) & 0xFF) as f32;
    let b = (fill & 0xFF) as f32;
    let lum = 0.299 * r + 0.587 * g + 0.114 * b;
    // 阈值取 118（而不是 150）：陶土棕 #B07A4E 这类中等亮度的强调色亮度约 133，
    // 按 150 判会选白字，白字压在 85% 不透明的棕色上只有 2.6:1，小字看着发虚；
    // 改成深字后约 4.6:1。浅紫 / 浅蓝这类高亮强调色仍然落在深字一侧。
    if lum > 118.0 {
        egui::Color32::from_rgb(0x1C, 0x1A, 0x1F)
    } else {
        egui::Color32::WHITE
    }
}

// ---------- 开关类控件的主题化配色 ----------
//
// 早先这些按钮直接用 Blue 600 + 白字，只有深色主题下能看：浅色主题（Calm 浅色 /
// M3 浅色）上「关」态的白 20% 底几乎看不出，「开」态的蓝底白字又和奶油底打架。
// 统一改成从当前主题派生：底色取强调色 / 正文色的低透明度，文字色按底色亮度反选，
// 这样 6 套主题（含浅色）都不需要单独调。

/// 「开」态底色：主题强调色，压到 85% 不透明（浅色主题的强调色偏浅，太透会糊进背景）
fn on_fill() -> egui::Color32 {
    argb((0xD9 << 24) | (cfg::cur().accent & 0x00FF_FFFF))
}

/// 「开」态文字：按强调色亮度选深字 / 浅字
fn on_text() -> egui::Color32 {
    ink_on(cfg::cur().accent)
}

/// 「关」态底色：正文色做 12% 中性底（深色主题=淡白，浅色主题=淡黑，两边都看得出边界）
fn off_fill() -> egui::Color32 {
    argb((0x1F << 24) | (cfg::cur().text & 0x00FF_FFFF))
}

/// 「关」态文字：正文色压到 55%
fn off_text() -> egui::Color32 {
    argb(cfg::cur().text).gamma_multiply(0.62)
}

/// 成功/已连通这类语义绿：深色主题用亮绿，浅色主题换深绿才够对比
fn ok_green() -> egui::Color32 {
    argb(if cfg::cur().dark {
        0xFF_3D_D6_8C
    } else {
        0xFF_1B_7F_45
    })
}

fn shade(c: u32, k: f32) -> u32 {
    let ch = |v: u32| ((v as f32 * k).round().clamp(0.0, 255.0)) as u32;
    (c & 0xFF00_0000) | (ch((c >> 16) & 0xFF) << 16) | (ch((c >> 8) & 0xFF) << 8) | ch(c & 0xFF)
}

/// 字体：与其余窗口共用同一份定义（MiSans 优先，系统字体兜底）
fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(crate::egui_soft::build_font_definitions());
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
    let border = argb(t.panel_border);
    // 控件底：从面板底色派生一档"抬起"色。深色主题要更亮、浅色主题要更暗，
    // 都用同一个 k→色 的映射容易在浅色主题上糊成一片，所以两边分开取系数。
    let raised = argb(shade(t.panel_bg, if t.dark { 1.14 } else { 0.94 }));
    // hover 走主题 hover 色（本来就带低透明度），再压一层同色系实底，
    // 免得浅色主题下 0x0E 的 alpha 几乎看不出"指到哪一行"
    let hover_solid = argb(shade(t.panel_bg, if t.dark { 1.22 } else { 0.90 }));
    ctx.set_theme(th);
    ctx.style_mut_of(th, |st| {
        let v = &mut st.visuals;
        v.panel_fill = panel;
        v.window_fill = panel;
        // 全局文字色统一用主题正文色：所有 RichText 不加 color 时都由此兜底，
        // 浅色主题下才不会继承 egui 深色默认的白字
        v.override_text_color = Some(text_body);
        v.selection.bg_fill = accent;
        // 选中项的文字色跟着强调色的亮度走（否则浅紫底 + 白字读不清）。
        // 宽度不能给 0：TextEdit 的选中文字就是用它画的，0 = 看不见。
        v.selection.stroke = egui::Stroke::new(1.0, ink_on(t.accent));
        // 普通按钮 / 输入框轨道：抬起色，保证在面板上分得清边界
        v.widgets.inactive.bg_fill = raised;
        v.widgets.inactive.weak_bg_fill = raised;
        v.widgets.noninteractive.bg_fill = raised;
        v.widgets.noninteractive.weak_bg_fill = raised;
        v.widgets.hovered.bg_fill = hover_solid;
        v.widgets.hovered.weak_bg_fill = hover_solid;
        v.widgets.active.bg_fill = hover_solid;
        v.widgets.active.weak_bg_fill = hover_solid;
        // 描边统一用主题分隔线色：浅色主题下默认的中灰描边会显脏
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, border);
        v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border);
        v.window_corner_radius = egui::CornerRadius::same(WIN_RADIUS as u8);
        v.window_stroke = egui::Stroke::new(1.0, border);
        // 次级/说明文字（`.weak()`）的专用色。
        //
        // egui 默认是「正文色 × weak_text_alpha(0.6)」，浅色主题下压到 0.54 的
        // 透明度，压在白底上只有 3.1:1，说明文字看着发灰发虚。这里显式给一个
        // 透明度更高的同色：浅色 72%、深色 66%，两边的对比度都能到 5:1 左右。
        v.weak_text_color = Some(argb(
            ((if t.dark { 0xA8 } else { 0xB8 }) << 24) | (t.text & 0x00FF_FFFF),
        ));
        // 输入框/滑条槽：深色主题再暗一档才有凹陷感；浅色主题反向往亮推，
        // 让输入框从奶油面板上"浮"出来（再配合上面的 border 描边就有边界了）
        v.extreme_bg_color = argb(shade(t.panel_bg, if t.dark { 0.86 } else { 1.04 }));
        // 非交互文字兜底：次级文字色（别用 divider——那个带 0x12 alpha，当文字用会看不见）
        v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, argb(t.text_dim));
    });
}

// ---------- 应用状态 ----------

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Group {
    #[default]
    General,
    Pill,
    Notify,
    Modules,
    Dock,
    Start,
    Wechat,
}

impl Group {
    const ALL: [Group; 7] = [
        Group::General,
        Group::Pill,
        Group::Notify,
        Group::Modules,
        Group::Dock,
        Group::Start,
        Group::Wechat,
    ];
    fn label(self) -> &'static str {
        match self {
            Group::General => "常规",
            Group::Pill => "顶部胶囊",
            Group::Notify => "通知",
            Group::Modules => "功能模块",
            Group::Dock => "Dock",
            Group::Start => "开始菜单",
            Group::Wechat => "微信",
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
    /// 上次安装字体的版本号（字体切换后要重建当前窗口的字体图集）
    seen_font_version: u64,
    /// 编辑框的本地缓冲（失焦且版本变化时才从 settings 重读，打字时不被写盘值顶掉）
    edits: Edits,
    /// 当前选中的分组
    group: Group,
    /// 邮箱测试连通状态（与后台线程共享）
    test: Arc<Mutex<TestState>>,
    /// 编辑会话中已应用预览的主题索引（主题改动需立即刷本窗样式）
    preview_theme: usize,
    /// 编辑会话中已应用预览的字体开关（字体改动需立即重建本窗图集）
    preview_misans: bool,
    /// 正在编辑的邮箱账号下标（None = 没在编辑，只显示账号列表）
    mail_edit: Option<usize>,
    /// 「浏览程序…」添加开始菜单快捷方式时是否以管理员权限
    pin_admin: bool,
    /// 微信快照操作的反馈信息（保存/恢复/快捷方式/删除的结果）
    snap_status: String,
}

#[derive(Default)]
struct Edits {
    clock: String,
    wechat_ignore: String,
    wechat_hotkeys: String,
    /// 账号列表每行的热键输入缓冲：账号快照 id → 文本（空 = 没绑）
    wechat_hk: std::collections::HashMap<String, String>,
    wechat_path: String,
    wechat_data_dir: String,
    snap_name: String,
    banner_secs: String,
    banner_secs_wechat: String,
    banner_secs_mail: String,
    banner_secs_tray: String,
    banner_secs_clipboard: String,
    clipboard_pinned: String,
    amap: String,
    host: String,
    port: String,
    user: String,
    pass: String,
}

impl eframe::App for SettingsApp {
    /// 首帧可见前的清屏色：跟主题面板底色一致，消除"先白一下再显示"的闪屏
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        argb(cfg::cur().panel_bg).to_normalized_gamma_f32()
    }

    /// 隐藏时 eframe 也会定期醒一次：只处理关窗（关闭即进程退出）
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 窗口首次可见由 eframe 首帧渲染后自动完成（clear_color 已对齐面板底色）。
        // 这里只管关窗：用户点关闭 / 主进程发 WM_CLOSE，都走「丢弃未应用改动 + 退出」。
        if ctx.input(|i| i.viewport().close_requested()) && !CLOSE_REQ.load(Ordering::Relaxed) {
            self.do_hide(ctx);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ver = settings::version();
        let ver_changed = ver != self.seen_version;
        if self.seen_version == 0 {
            // 子进程启动即进入编辑会话：改动先攒内存，点「应用」才落盘
            settings::begin_edit();
            self.load_edits();
            self.preview_theme = settings::theme_index();
            self.preview_misans = settings::use_misans();
        }
        // 主题（或任何设置）变了就重刷原生控件样式；只在变化时做，避免每帧 clone Style。
        // 编辑会话里 bump 被吞、ver 不变，但主题改动要立即预览，所以单独比对主题索引。
        let theme_idx = settings::theme_index();
        if ver_changed || self.seen_style_version == 0 || theme_idx != self.preview_theme {
            apply_style(&ctx);
            self.seen_style_version = ver;
            self.preview_theme = theme_idx;
        }
        // 字体切换（MiSans / 系统）后立刻重建本窗口的字体图集
        let misans = settings::use_misans();
        if ver_changed || self.seen_font_version == 0 || misans != self.preview_misans {
            install_fonts(&ctx);
            self.seen_font_version = ver;
            self.preview_misans = misans;
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(argb(cfg::cur().panel_bg))
                    .inner_margin(14.0),
            )
            .show(ui, |ui| {
                // 用「顶栏 + 左侧导航 + 内容」三段面板：内容区的 ScrollArea 才能拿到
                // 整块剩余高度。早先塞在 `ui.horizontal` 里时，它的可用高度会被
                // 前面那块导航列"传染"，右栏只剩两百来像素、下面内容全被裁掉。
                egui::Panel::top("set_hdr")
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        self.header(ui, &ctx);
                        ui.add_space(6.0);
                    });

                egui::Panel::left("set_nav")
                    .resizable(false)
                    .exact_size(NAV_W + 8.0)
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(NAV_W);
                            for g in Group::ALL {
                                let sel = self.group == g;
                                // 选中项用真正的粗体（MiSans Bold / 雅黑 Bold），
                                // `.strong()` 在 egui 0.37 里只改颜色不改字重。
                                let txt = if sel {
                                    egui_soft::bold(g.label()).size(13.0).color(ink_on(cfg::cur().accent))
                                } else {
                                    egui::RichText::new(g.label()).size(13.0).color(argb(cfg::cur().text_body))
                                };
                                if ui
                                    .add_sized([NAV_W, 30.0], egui::Button::selectable(sel, txt))
                                    .clicked()
                                {
                                    self.group = g;
                                }
                            }
                        });
                        // 导航列右侧的竖线（原来是 horizontal 里的 separator）
                        let r = ui.max_rect();
                        ui.painter().vline(
                            r.right() - 1.0,
                            r.y_range(),
                            egui::Stroke::new(1.0, argb(cfg::cur().divider)),
                        );
                    });

                egui::CentralPanel::no_frame().show(ui, |ui| {
                    // 右侧内容离中间竖线留出内边距：之前实测只有约 10px，
                    // 看起来像贴在线上。这里统一给 16px，滚动时不会跟着晃。
                    egui::Frame::NONE
                        .inner_margin(egui::Margin {
                            left: 16,
                            right: 8,
                            top: 2,
                            bottom: 0,
                        })
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                                        // 取滚动区自己的可用宽度：出滚动条后要自动减掉，
                                        // 否则右侧「移除 / 指定图标…」按钮会被挤出可视区
                                        ui.set_width(ui.available_width());
                                        self.detail(ui, ver_changed);
                                    });
                                });
                        });
                });
            });

        self.seen_version = ver;
    }
}

impl SettingsApp {
    fn do_hide(&mut self, ctx: &egui::Context) {
        // 关窗 = 放弃未应用改动，从文件恢复
        settings::discard_edit();
        CLOSE_REQ.store(true, Ordering::Relaxed);
        // 直接请求关闭视口：eframe 退出事件循环 → 进程退出、
        // GL 上下文与全部 UI 内存随之归还（比常驻 hide 省几十 MB）
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// 主屏居中已由 `NativeOptions::centered` 在**建窗前**完成（见 run_child）：
    /// 那里按 monitor 尺寸算好位置再建窗，不会出现"先出现在左上角再跳中间"的闪动。
    fn load_edits(&mut self) {
        self.edits.clock = settings::clock_format();
        self.edits.wechat_ignore = settings::wechat_ignore_text();
        self.edits.wechat_hotkeys = settings::wechat_hotkeys_text();
        self.edits.wechat_hk = crate::wechatsnap::list()
            .into_iter()
            .map(|s| (s.id.clone(), settings::wechat_hotkey_for(&s.id)))
            .collect();
        self.edits.wechat_path = settings::wechat_path();
        self.edits.wechat_data_dir = settings::wechat_data_dir();
        self.edits.banner_secs = settings::banner_secs().to_string();
        // 四个来源的输入框也要一起初始化：以前只填了 banner_secs，另外四个空着 →
        // 打开页面看到的是 hint 里的「6」，要等点进输入框失焦一次才读本地值
        self.edits.banner_secs_wechat = settings::banner_secs_wechat().to_string();
        self.edits.banner_secs_mail = settings::banner_secs_mail().to_string();
        self.edits.banner_secs_tray = settings::banner_secs_tray().to_string();
        self.edits.banner_secs_clipboard = settings::banner_secs_clipboard().to_string();
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
            Group::General => self.detail_general(ui, ver_changed),
            Group::Pill => self.detail_pill(ui, ver_changed),
            Group::Notify => self.detail_notify(ui, ver_changed),
            Group::Modules => self.detail_modules(ui, ver_changed),
            Group::Dock => self.detail_dock(ui),
            Group::Start => self.detail_start(ui),
            Group::Wechat => self.detail_wechat(ui, ver_changed),
        }
    }

    /// 常规设置：字体 / 外观（主题 + 透明度·大小·形状·位置）/ 系统（开机启动、退出守护）
    fn detail_general(&mut self, ui: &mut egui::Ui, _ver_changed: bool) {
        // 字体放最上面：一换字体整页字全变，第一眼就能看出效果
        section(ui, "界面字体");
        if let Some(pick) = seg(
            ui,
            &["MiSans", "系统字体"],
            if settings::use_misans() { 0 } else { 1 },
        ) {
            settings::set_use_misans(pick == 0);
            crate::tlog!("[settings] 界面字体 → {}", if settings::use_misans() { "MiSans" } else { "系统字体" });
        }
        ui.label(
            egui::RichText::new("MiSans 随程序目录分发（data/MiSans-Semibold.ttf，缺失时回落系统字体）；系统字体 = 微软雅黑等本机字体")
                .size(11.5)
                .weak(),
        );

        ui.add_space(10.0);
        ui.add_space(10.0);
        section(ui, "外观");
        // 外观主题：一排小色块，直接用主题自己的底色画，所见即所得
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

        // 透明度 / 胶囊大小 / 胶囊形状 / Dock 位置：都属于"外观"，统一放在主题下面。
        // 透明度是**一个值管所有界面**（胶囊 / 模块条 / 音乐条 / Dock / 面板 /
        // 开始菜单 / 横幅），天气卡片不受影响。
        ui.add_space(8.0);
        egui::Grid::new("g_pill_look")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                // 界面透明度：滑条（拖动时只做即时预览，松手才落盘）
                let mut alpha = settings::ui_alpha();
                ui.label(row_label("界面透明度"));
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
                        settings::preview_ui_alpha(alpha);
                    }
                    // 松手（或点击轨道、键盘微调这类一次性改动）才写盘
                    if r.drag_stopped() || (r.changed() && !r.dragged()) {
                        settings::persist_ui_alpha();
                    }
                    ui.label(
                        egui::RichText::new(format!("{:.0}%", settings::ui_alpha() * 100.0))
                            .size(12.5)
                            .monospace(),
                    );
                });
                ui.end_row();

                // 圆角：胶囊 / 音乐条 / 模块条 / Dock 共用（默认 14px = 圆角矩形）。
                // 原在「顶部胶囊」页，挪到透明度下面统一管外观；落盘后主动通知
                // 主进程的胶囊 + Dock 立即重绘，不等 500ms 轮询
                ui.label(row_label("圆角"));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.spacing_mut().slider_width = CTRL_W - 148.0;
                    let mut cr = settings::corner_radius();
                    let r = ui.add(
                        egui::Slider::new(&mut cr, settings::CORNER_MIN..=settings::CORNER_MAX)
                            .show_value(false)
                            .trailing_fill(true),
                    );
                    if r.changed() {
                        settings::preview_corner_radius(cr);
                    }
                    if r.drag_stopped() || (r.changed() && !r.dragged()) {
                        settings::persist_corner_radius();
                        post_to_parent(crate::appmsg::WM_SETTINGS_APPLIED);
                    }
                    ui.label(
                        egui::RichText::new(format!("{:.0}px", settings::corner_radius()))
                            .size(12.5)
                            .monospace(),
                    );
                    let at_default = (settings::corner_radius() - 14.0).abs() < 0.5;
                    if ui
                        .add_enabled(!at_default, egui::Button::new("重置").small())
                        .on_hover_text("回到圆角矩形（14px）")
                        .clicked()
                    {
                        settings::set_corner_radius(14.0);
                        post_to_parent(crate::appmsg::WM_SETTINGS_APPLIED);
                    }
                });
                ui.end_row();

                ui.label(row_label("胶囊大小"));
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

                // Dock 大小（原「Dock」分组里的图标大小）：跟胶囊大小挨着，一起比着调
                ui.label(row_label("Dock 大小"));
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

                // 胶囊形状（原「顶部胶囊」分组里的第一项）
                ui.label(row_label("胶囊形状"));
                let shape = match settings::pill_anchor() {
                    PillAnchor::Float => 0,
                    PillAnchor::Island => 1,
                };
                if let Some(i) = seg(ui, &["悬浮胶囊", "吸顶刘海"], shape) {
                    settings::set_pill_anchor(match i {
                        1 => PillAnchor::Island,
                        _ => PillAnchor::Float,
                    });
                }
                ui.end_row();

                // Dock 位置（原「Dock」分组里的第一项）
                ui.label(row_label("Dock 位置"));
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
            });
        ui.label(
            egui::RichText::new("界面透明度只调这些界面的背景底色（胶囊 / 模块条 / 音乐条 / Dock / 面板 / 开始菜单 / 横幅，天气卡片除外），文字与图标始终不透明；两个「大小」各自只缩放那一块")
                .size(11.5)
                .weak(),
        );

        ui.add_space(10.0);
        section(ui, "系统");
        // 开机启动
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let on = settings::autostart();
            if toggle_btn(ui, on, "开机启动") {
                settings::set_autostart(!on);
                // 注册表立即对齐（不走「应用」，开关即生效）
                crate::guard::apply_autostart(settings::autostart());
                crate::tlog!(
                    "[settings] 开机启动 → {}",
                    if settings::autostart() { "开启" } else { "关闭" }
                );
            }
            ui.label(
                egui::RichText::new("开机自启动（Run 键 + 启动文件夹 + 计划任务三通道；计划任务需弹一次 UAC）")
                    .size(11.5)
                    .weak(),
            );
        });
        ui.add_space(6.0);
        // 退出守护
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let on = settings::guardian();
            if toggle_btn(ui, on, "退出守护") {
                settings::set_guardian(!on);
                crate::tlog!(
                    "[settings] 退出守护 → {}",
                    if settings::guardian() { "开启" } else { "关闭" }
                );
            }
            ui.label(
                egui::RichText::new("orbit 被强杀（任务栏没恢复）时自动重启 explorer 拉回任务栏")
                    .size(11.5)
                    .weak(),
            );
        });
        ui.label(
            egui::RichText::new("守护随 orbit 启动；开关在下次启动 orbit 时生效。正常退出（Ctrl+Alt+O）不触发复活；异常 / panic 退出时守护会自动复活 orbit（连续最多 3 次，复活成功或正常退出后计数清零）")
                .size(11.5)
                .weak(),
        );
    }

    /// 通知：各类消息的通知方式 / 接管系统通知 / 横幅显示时长 / 横幅大小 / 提示音
    fn detail_notify(&mut self, ui: &mut egui::Ui, _ver_changed: bool) {
        section(ui, "通知方式");
        ui.label(
            egui::RichText::new("每类消息各自选择弹在哪里：Orbit 横幅（右下角卡片）/ Windows 系统通知 / 不通知")
                .size(11.5)
                .weak(),
        );
        egui::Grid::new("g_notify_ch")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                for (name, ch) in [
                    ("微信", settings::notify_wechat()),
                    ("邮件", settings::notify_mail()),
                    ("托盘闪烁", settings::notify_tray()),
                    ("剪切板", settings::notify_clipboard()),
                ] {
                    ui.label(row_label(name));
                    let idx = match ch {
                        settings::NotifyChannel::Orbit => 0,
                        settings::NotifyChannel::Windows => 1,
                        settings::NotifyChannel::Off => 2,
                    };
                    if let Some(i) = seg(ui, &["Orbit 横幅", "Windows 通知", "不通知"], idx) {
                        let c = match i {
                            1 => settings::NotifyChannel::Windows,
                            2 => settings::NotifyChannel::Off,
                            _ => settings::NotifyChannel::Orbit,
                        };
                        match name {
                            "微信" => settings::set_notify_wechat(c),
                            "邮件" => settings::set_notify_mail(c),
                            "托盘闪烁" => settings::set_notify_tray(c),
                            _ => settings::set_notify_clipboard(c),
                        }
                    }
                    ui.end_row();
                }
            });

        ui.add_space(10.0);
        ui.add_space(10.0);
        section(ui, "接管系统通知");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let on = settings::notify_takeover();
            if toggle_btn(ui, on, "接管通知中心") {
                settings::set_notify_takeover(!on);
                crate::tlog!(
                    "[settings] 接管系统通知 → {}",
                    if settings::notify_takeover() { "开启" } else { "关闭" }
                );
            }
            ui.label(
                egui::RichText::new("其它程序的 Windows 通知改在 Orbit 横幅里显示，并从通知中心撤掉")
                    .size(11.5)
                    .weak(),
            );
        });
        // 授权结果由主进程的监听线程写进 data/notify-access.txt，子进程读同一份
        ui.label(
            egui::RichText::new(crate::notifycenter::access_hint())
                .size(11.0)
                .weak(),
        );

        ui.add_space(10.0);
        section(ui, "横幅显示时长");
        ui.label(
            egui::RichText::new("每类横幅自动关闭的秒数；0 = 不自动关闭，点一下才收起")
                .size(11.5)
                .weak(),
        );
        egui::Grid::new("g_banner_secs")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                for (name, key) in [
                    ("微信", 0u8),
                    ("邮件", 1u8),
                    ("托盘闪烁", 2u8),
                    ("剪切板", 3u8),
                ] {
                    ui.label(row_label(name));
                    let buf = match key {
                        0 => &mut self.edits.banner_secs_wechat,
                        1 => &mut self.edits.banner_secs_mail,
                        2 => &mut self.edits.banner_secs_tray,
                        _ => &mut self.edits.banner_secs_clipboard,
                    };
                    let cur = match key {
                        0 => settings::banner_secs_wechat(),
                        1 => settings::banner_secs_mail(),
                        2 => settings::banner_secs_tray(),
                        _ => settings::banner_secs_clipboard(),
                    };
                    let r = ui.add_sized([CTRL_W, 24.0], egui::TextEdit::singleline(buf).hint_text("6"));
                    // 逐键提交：设置窗失活会直接收起，文本框来不及触发 lost_focus
                    let parsed = buf.trim().parse::<u32>().ok();
                    if r.changed() {
                        if let Some(v) = parsed {
                            match key {
                                0 => settings::set_banner_secs_wechat(v),
                                1 => settings::set_banner_secs_mail(v),
                                2 => settings::set_banner_secs_tray(v),
                                _ => settings::set_banner_secs_clipboard(v),
                            }
                        }
                    }
                    if r.lost_focus() {
                        let v = parsed.unwrap_or(6);
                        match key {
                            0 => settings::set_banner_secs_wechat(v),
                            1 => settings::set_banner_secs_mail(v),
                            2 => settings::set_banner_secs_tray(v),
                            _ => settings::set_banner_secs_clipboard(v),
                        }
                        *buf = cur.to_string();
                    }
                    ui.end_row();
                }
            });

        ui.add_space(10.0);
        section(ui, "横幅大小");
        ui.label(
            egui::RichText::new("拖动滑块调整右下角横幅的整体大小，屏幕右下角会弹预览卡实时看效果")
                .size(11.5)
                .weak(),
        );
        scale_row(
            ui,
            settings::banner_scale_setting(),
            settings::BANNER_SCALE_MIN,
            settings::BANNER_SCALE_MAX,
            |v| {
                settings::preview_banner_scale(v);
                crate::banner::show_preview(); // 拖动即弹样例卡，原位换新尺寸
            },
            settings::persist_banner_scale,
            || {
                settings::set_banner_scale(1.0);
                crate::banner::show_preview(); // 重置后也弹一张 100% 的样例卡
            },
        );

        ui.add_space(10.0);
        section(ui, "提示音");
        let mode = settings::sound_mode();
        let idx = match mode {
            settings::SoundMode::Off => 0,
            settings::SoundMode::Once => 1,
            settings::SoundMode::Twice => 2,
            settings::SoundMode::Thrice => 3,
            settings::SoundMode::UntilDismiss => 4,
        };
        if let Some(i) = seg(
            ui,
            &["关", "一声", "两声", "三声", "循环到横幅消失"],
            idx,
        ) {
            let m = match i {
                1 => settings::SoundMode::Once,
                2 => settings::SoundMode::Twice,
                3 => settings::SoundMode::Thrice,
                4 => settings::SoundMode::UntilDismiss,
                _ => settings::SoundMode::Off,
            };
            settings::set_sound_mode(m);
            crate::tlog!("[settings] 提示音模式 → {m:?}");
        }
        ui.label(
            egui::RichText::new("「循环到横幅消失」：横幅常驻期间提示音循环播放，横幅收起即停")
                .size(11.5)
                .weak(),
        );
    }

    /// 开始菜单：Win 键接管 + 快捷方式（排序 / 每行数量 / 管理员启动）
    fn detail_start(&mut self, ui: &mut egui::Ui) {
        section(ui, "Win 键接管");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let on = settings::start_hook();
            if toggle_btn(ui, on, "接管 Win 键") {
                settings::set_start_hook(!on);
                crate::tlog!(
                    "[settings] Win 键接管 → {}",
                    if settings::start_hook() { "开启" } else { "关闭" }
                );
            }
            ui.label(
                egui::RichText::new("按下 Win 弹出 Orbit 开始菜单；关闭后 Win 键交还 Windows 系统开始菜单")
                    .size(11.5)
                    .weak(),
            );
        });

        ui.add_space(10.0);
        section(ui, "开始菜单快捷方式");
        ui.label(
            egui::RichText::new(format!(
                "上栏每行 {} 个（最多两行）；拖动行首圆点或点 ↑↓ 调整顺序。路径前加 @admin: 表示以管理员权限启动",
                settings::start_cols()
            ))
            .size(11.5)
            .weak(),
        );
        egui::Grid::new("g_start_cols")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label(row_label("每行个数"));
                let cols = settings::start_cols();
                if let Some(d) = stepper(ui, &format!("{cols}"), cols > 1, cols < 8) {
                    settings::set_start_cols((cols as i32 + d as i32).max(1) as usize);
                }
                ui.end_row();
            });

        let pins = settings::start_pins();
        if pins.is_empty() {
            ui.label(
                egui::RichText::new("还没有快捷方式，点「浏览程序…」添加")
                    .size(11.5)
                    .weak(),
            );
        }
        let n = pins.len();
        // 拖动排序：全局 payload 记「正在拖哪一行」，拖到别的行上实时交换
        let dragging = egui::DragAndDrop::payload::<usize>(ui.ctx()).map(|p| *p);
        let mut row_rects: Vec<(usize, egui::Rect)> = Vec::new();
        for (i, (name, target)) in pins.iter().enumerate() {
            let admin = target
                .strip_prefix("@admin:")
                .map(|p| p.to_string())
                .unwrap_or_else(|| target.clone());
            let row = ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                // 拖动把手：2×3 圆点 grip（手绘，不依赖字体字形）。按住拖到目标行即可换位
                let (hr, hresp) = ui.allocate_exact_size(egui::vec2(14.0, 18.0), egui::Sense::drag());
                let dot_color = if dragging == Some(i) {
                    argb(cfg::cur().accent)
                } else {
                    argb(cfg::cur().text_dim)
                };
                let c = hr.center();
                for dx in [-3.0f32, 3.0] {
                    for dy in [-4.5f32, 0.0, 4.5] {
                        ui.painter().circle_filled(c + egui::vec2(dx, dy), 1.2, dot_color);
                    }
                }
                let hresp = hresp.on_hover_text("按住拖动调整顺序");
                if hresp.drag_started() {
                    egui::DragAndDrop::set_payload(ui.ctx(), i);
                }
                if hresp.drag_stopped() {
                    egui::DragAndDrop::clear_payload(ui.ctx());
                }
                hresp.on_hover_cursor(egui::CursorIcon::Grab);
                ui.label(
                    egui::RichText::new(format!("{}.", i + 1))
                        .size(11.5)
                        .color(argb(cfg::cur().text_dim)),
                );
                ui.label(egui::RichText::new(name.as_str()).size(12.5));
                if target.starts_with("@admin:") {
                    ui.label(
                        egui::RichText::new("[管理员]")
                            .size(11.0)
                            .color(argb(cfg::cur().accent)),
                    );
                }
                ui.label(
                    egui::RichText::new(admin.as_str())
                        .size(11.0)
                        .color(argb(cfg::cur().text_dim)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("移除").small()).clicked() {
                        let mut list: Vec<(String, String)> = settings::start_pins();
                        list.remove(i);
                        settings::set_start_pins(list);
                    }
                    if ui
                        .add_enabled(i > 0, egui::Button::new("↑").small())
                        .on_hover_text("上移")
                        .clicked()
                    {
                        let mut list = settings::start_pins();
                        list.swap(i - 1, i);
                        settings::set_start_pins(list);
                    }
                    if ui
                        .add_enabled(i + 1 < n, egui::Button::new("↓").small())
                        .on_hover_text("下移")
                        .clicked()
                    {
                        let mut list = settings::start_pins();
                        list.swap(i, i + 1);
                        settings::set_start_pins(list);
                    }
                });
            });
            row_rects.push((i, row.response.rect));
        }

        // 拖着走的行悬到另一行上：实时交换顺序（松手即定格）
        if let Some(src) = dragging {
            if let Some(pos) = ui.ctx().input(|i| i.pointer.latest_pos()) {
                if let Some(&(j, _)) = row_rects.iter().find(|(_, r)| r.contains(pos)) {
                    if j != src {
                        let mut list = settings::start_pins();
                        if src < list.len() && j < list.len() {
                            let item = list.remove(src);
                            list.insert(j, item);
                            settings::set_start_pins(list);
                            egui::DragAndDrop::set_payload(ui.ctx(), j);
                        }
                    }
                }
            }
        }

        ui.add_space(6.0);
        // 常用程序一键添加：Windows 里没有「任务管理器」的开始菜单项，想加到
        // Orbit 开始菜单只能自己翻路径找 Taskmgr.exe，这里给个现成入口。
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(
                egui::RichText::new("快速添加")
                    .size(11.5)
                    .color(argb(cfg::cur().text_dim)),
            );
            let taskmgr = settings::taskmgr_path();
            let target = taskmgr.as_ref().map(|p| p.to_string_lossy().to_string());
            let added = target
                .as_ref()
                .map_or(false, |t| pins.iter().any(|(_, p)| p.eq_ignore_ascii_case(t)));
            let full = n >= settings::start_max_pins();
            let r = ui
                .add_enabled(
                    taskmgr.is_some() && !added && !full,
                    egui::Button::new("任务管理器").small(),
                )
                .on_hover_text(if taskmgr.is_none() {
                    "没找到 Taskmgr.exe"
                } else if added {
                    "已经在列表里了"
                } else if full {
                    "开始菜单格子已满，先移除一条再加"
                } else {
                    "在开始菜单里加一条「任务管理器」快捷方式（Taskmgr.exe）"
                });
            if r.clicked() {
                if let Some(t) = target {
                    let mut list = settings::start_pins();
                    list.push(("任务管理器".to_string(), t));
                    settings::set_start_pins(list);
                    crate::tlog!("[settings] 开始菜单添加 → 任务管理器");
                }
            }
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new("浏览程序…").small()).clicked() {
                if let Some(path) = pick_program(false) {
                    let target = if self.pin_admin {
                        format!("@admin:{path}")
                    } else {
                        path.clone()
                    };
                    let mut list = settings::start_pins();
                    if !list.iter().any(|(_, p)| p.eq_ignore_ascii_case(&target)) {
                        let name = std::path::Path::new(&path)
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.clone());
                        list.push((name, target));
                        settings::set_start_pins(list);
                    }
                }
            }
            let mut admin = self.pin_admin;
            if ui
                .checkbox(&mut admin, "以管理员权限添加")
                .on_hover_text("勾选后添加的快捷方式以管理员权限（UAC 提权）启动")
                .changed()
            {
                self.pin_admin = admin;
            }
            if ui
                .add(egui::Button::new("恢复默认").small())
                .on_hover_text("恢复为「命令提示符（管理员）」一条")
                .clicked()
            {
                settings::set_start_pins(vec![
                    (
                        "命令提示符(管理员)".to_string(),
                        "@admin:C:\\Windows\\System32\\cmd.exe".to_string(),
                    ),
                ]);
            }
        });
    }

    /// 微信分组：消息监控 / 程序路径与数据目录 / 登录凭证快照。
    /// （微信的通知方式与横幅时长已并入「通知」分组）
    fn detail_wechat(&mut self, ui: &mut egui::Ui, ver_changed: bool) {
        section(ui, "消息监控");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let on = settings::wechat_cards();
            if toggle_btn(ui, on, "微信卡片") {
                settings::set_wechat_cards(!on);
                crate::tlog!(
                    "[settings] 微信消息卡片 → {}",
                    if settings::wechat_cards() { "开启" } else { "关闭" }
                );
            }
            ui.label(
                egui::RichText::new(
                    "开启后只读微信聊天列表（UIA，不改动微信本体），新消息弹会话名+摘要卡片，读不到的实例退回「有新消息」横幅；关闭时托盘图标闪烁也弹「有新消息」横幅（点击跳回微信）。两种方式的横幅时长与提示音设置通用",
                )
                .size(11.5)
                .weak(),
            );
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let jump = settings::wechat_banner_jump();
            if toggle_btn(ui, jump, "横幅直达") {
                settings::set_wechat_banner_jump(!jump);
                crate::tlog!(
                    "[settings] 点横幅直达会话 → {}",
                    if settings::wechat_banner_jump() { "开启" } else { "关闭" }
                );
            }
            ui.label(
                egui::RichText::new(
                    "点微信消息横幅（会话名 + 摘要那种）时，单击即切到那个对话（会把它标为已读）；关闭则只把微信窗口唤到前台，不动当前对话。默认关闭——开启后会向微信投递一次后台单击（不移动你的鼠标），若那时列表刚好因新消息重排，可能切到相邻会话。托盘闪烁的「有新消息」横幅没有具体会话，始终只唤窗口",
                )
                .size(11.5)
                .weak(),
            );
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let hp = settings::wechat_hover_preview();
            if toggle_btn(ui, hp, "悬停预览") {
                settings::set_wechat_hover_preview(!hp);
                crate::tlog!(
                    "[settings] 微信悬停预览 → {}",
                    if settings::wechat_hover_preview() { "开启" } else { "关闭" }
                );
            }
            ui.label(
                egui::RichText::new(
                    "用 Orbit 自绘的浮窗替代微信自带的托盘悬停小窗：鼠标停在 Dock 的微信图标上时列出全部未读会话（会话名 + 摘要 + 未读数），点一条直达。微信收进托盘时读不到未读数，这种情况下仍用微信自己的小窗",
                )
                .size(11.5)
                .weak(),
            );
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(row_label("不弹白名单"));
            let r = ui.add_sized(
                [CTRL_W, 24.0],
                egui::TextEdit::singleline(&mut self.edits.wechat_ignore)
                    .hint_text("可添加多个：标题之间用英文逗号 , 分隔"),
            );
            if r.lost_focus() {
                // 英文逗号分隔
                let list: Vec<String> = self
                    .edits
                    .wechat_ignore
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                settings::set_wechat_ignore(list);
                self.edits.wechat_ignore = settings::wechat_ignore_text();
                crate::tlog!("[settings] 微信卡片白名单 → {}", settings::wechat_ignore_text());
            }
            if ver_changed && !r.has_focus() {
                self.edits.wechat_ignore = settings::wechat_ignore_text();
            }
        });
        ui.label(
            egui::RichText::new(
                "可添加多个标题（英文逗号分隔，完全匹配才跳过）；被跳过的标题会写进 orbit.log",
            )
            .size(11.5)
            .weak(),
        );

        // 微信热键（显示 ↔ 最小化）：英文逗号分隔，按微信启动先后对应
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(row_label("微信热键"));
            let r = ui.add_sized(
                [CTRL_W, 24.0],
                egui::TextEdit::singleline(&mut self.edits.wechat_hotkeys)
                    .hint_text("例：F1,F2 或 Ctrl+Alt+1,Ctrl+Alt+2"),
            );
            if r.lost_focus() {
                let list: Vec<String> = self
                    .edits
                    .wechat_hotkeys
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                settings::set_wechat_hotkeys(list);
                self.edits.wechat_hotkeys = settings::wechat_hotkeys_text();
                crate::tlog!("[settings] 微信热键 → {}", settings::wechat_hotkeys_text());
            }
            if ver_changed && !r.has_focus() {
                self.edits.wechat_hotkeys = settings::wechat_hotkeys_text();
            }
        });
        ui.label(
            egui::RichText::new(
                "按微信启动先后对应：第 1 个热键 = 先启动的微信（没有账号快照的实例用这条兜底；有快照的账号请到下表的行内热键框里填，跟账号走）。动作只有显示↔最小化，绝不隐藏到托盘；注册失败会弹横幅告知",
            )
            .size(11.5)
            .weak(),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("手动拿 UIA").size(12.0))
                        .min_size(egui::vec2(96.0, 24.0)),
                )
                .clicked()
            {
                // 设置页是独立进程，抓 UIA 的动作必须让主进程做
                if IS_CHILD.load(Ordering::Relaxed) {
                    post_to_parent(crate::appmsg::WM_GRAB_UIA);
                } else {
                    crate::banner::request_grab();
                }
                crate::tlog!("[settings] 手动拿 UIA 已点击");
            }
            ui.label(
                egui::RichText::new("先唤出微信窗口（支持多开）再点；拿到后右下角提示一次")
                    .size(11.5)
                    .weak(),
            );
        });

        // 账号监控表：读各快照的 account.txt（昵称 / pid / 备份时间），备份时间
        // 最早的排 1 号。pid 还活着且 UIA 可读 = 已监控。
        // 「居中」= 唤醒该实例并把窗口移到屏幕正中；「已监控/未监控」按钮 =
        // 让主进程拿一次 UIA 树（与「手动拿 UIA」同一动作）。
        let snaps_mon = crate::wechatsnap::list();
        if !snaps_mon.is_empty() {
            ui.add_space(6.0);
            egui::Grid::new("g_wechat_accounts")
                .num_columns(6)
                .spacing([12.0, 4.0])
                // 序号列只放一个数字，不让它按 interact_size（默认 40px）撑开
                .min_col_width(0.0)
                .show(ui, |ui| {
                    for (i, s) in snaps_mon.iter().enumerate() {
                        let alive = crate::wechatsnap::logged_in(&s.id);
                        let mon = alive.map(|p| wechat_pid_readable(p)).unwrap_or(false);
                        let hk = alive.map(|p| wechat_pid_hotkey(p)).unwrap_or_default();
                        ui.add_sized(
                            egui::vec2(16.0, 18.0),
                            egui::Label::new(egui::RichText::new(format!("{}", i + 1)).size(12.0)),
                        );
                        ui.label(egui::RichText::new(s.nickname.as_str()).size(12.0));
                        ui.label(
                            egui::RichText::new(
                                crate::wechatsnap::stored_pid(&s.id)
                                    .map(|p| p.to_string())
                                    .unwrap_or_else(|| "—".into()),
                            )
                            .size(12.0),
                        );
                        // 居中：唤醒该实例（托盘态点托盘 / 最小化还原）→ 移到屏幕正中
                        if ui
                            .add_enabled(
                                alive.is_some(),
                                egui::Button::new(egui::RichText::new("居中").size(12.0))
                                    .min_size(egui::vec2(48.0, 22.0)),
                            )
                            .clicked()
                        {
                            if let Some(p) = alive {
                                post_to_parent_w(crate::appmsg::WM_CENTER_WECHAT, p as usize);
                                crate::tlog!("[settings] 请求居中微信 pid={p}");
                            }
                        }
                        // 热键：直接在这一行填（留空 = 不绑这个账号）。
                        // 绑的是账号快照 id，微信换启动顺序 / 换昵称都不会串
                        let mut buf = self
                            .edits
                            .wechat_hk
                            .get(&s.id)
                            .cloned()
                            .unwrap_or_else(|| settings::wechat_hotkey_for(&s.id));
                        let hint = if hk.is_empty() {
                            "填热键".to_string()
                        } else {
                            format!("顺序 {hk}")
                        };
                        let r = ui
                            .add_sized(
                                egui::vec2(96.0, 22.0),
                                egui::TextEdit::singleline(&mut buf).hint_text(hint),
                            )
                            .on_hover_text(
                                "该账号的「显示↔最小化」热键：写法 F1 / Ctrl+Alt+1，留空 = 不绑。\n绑到账号本身上（不看启动顺序），换昵称、换多开顺序都不受影响",
                            );
                        if r.changed() {
                            self.edits.wechat_hk.insert(s.id.clone(), buf.clone());
                        }
                        if r.lost_focus() {
                            settings::set_wechat_hotkey_for(&s.id, buf.trim());
                            let saved = settings::wechat_hotkey_for(&s.id);
                            self.edits.wechat_hk.insert(s.id.clone(), saved.clone());
                            crate::tlog!(
                                "[settings] 账号「{}」热键 → {:?}",
                                s.nickname,
                                if saved.is_empty() { "（空）" } else { &saved }
                            );
                        }
                        // 监控状态按钮：点一下让主进程拿一次 UIA 树
                        let st = if mon { "已监控" } else { "未监控" };
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(st)
                                        .size(12.0)
                                        .color(if mon { ok_green() } else { argb(cfg::cur().text_dim) }),
                                )
                                .min_size(egui::vec2(72.0, 22.0)),
                            )
                            .clicked()
                        {
                            post_to_parent(crate::appmsg::WM_GRAB_UIA);
                            crate::tlog!("[settings] 账号表请求拿 UIA（{}）", s.nickname);
                        }
                        ui.end_row();
                    }
                });
            ui.label(
                egui::RichText::new(
                    "「居中」= 唤醒微信（托盘态自动点托盘唤出 / 最小化自动还原）并把窗口移到屏幕上下、左右正中；「已监控 / 未监控」点一下 = 拿一次 UIA 树；热键框留空 = 这个账号不绑热键",
                )
                .size(11.0)
                .weak(),
            );
            if snaps_mon.iter().any(|s| {
                crate::wechatsnap::logged_in(&s.id)
                    .map(|p| !wechat_pid_readable(p))
                    .unwrap_or(false)
            }) {
                ui.label(
                    egui::RichText::new("未监控的账号：先点「居中」唤出它的微信主窗口，再点状态按钮拿一次 UIA")
                        .size(11.0)
                        .weak(),
                );
            }
        }

        ui.add_space(10.0);
        ui.add_space(10.0);
        section(ui, "微信程序");
        ui.horizontal(|ui| {
            ui.label(row_label("程序路径"));
            let r = ui.add_sized(
                [CTRL_W, 24.0],
                egui::TextEdit::singleline(&mut self.edits.wechat_path)
                    .hint_text("完整路径，需带 Weixin.exe；留空 = 自动探测"),
            );
            if r.lost_focus() {
                settings::set_wechat_path(&self.edits.wechat_path);
                crate::tlog!("[settings] 微信程序路径 → {:?}", settings::wechat_path());
            }
            if ver_changed && !r.has_focus() {
                self.edits.wechat_path = settings::wechat_path();
            }
        });
        ui.horizontal(|ui| {
            ui.label(row_label("数据目录"));
            let r = ui.add_sized(
                [CTRL_W, 24.0],
                egui::TextEdit::singleline(&mut self.edits.wechat_data_dir)
                    .hint_text("提供 xwechat_files 目录即可；留空 = 自动探测"),
            );
            if r.lost_focus() {
                settings::set_wechat_data_dir(&self.edits.wechat_data_dir);
                crate::tlog!("[settings] 微信数据目录 → {:?}", settings::wechat_data_dir());
            }
            if ver_changed && !r.has_focus() {
                self.edits.wechat_data_dir = settings::wechat_data_dir();
            }
        });
        ui.label(
            egui::RichText::new(format!(
                "检查：程序路径 {}；数据目录 {}（填好后凭证快照即可用）",
                match crate::wechatsnap::wechat_exe() {
                    Some(p) => format!("✓ {}", p.display()),
                    None => "✗ 未配置或文件不存在".into(),
                },
                match crate::wechatsnap::data_root() {
                    Some(p) => format!("✓ {}", p.display()),
                    None => "✗ 未配置或目录不对（需含 all_users/config/global_config）".into(),
                },
            ))
            .size(11.5)
            .weak(),
        );

        ui.add_space(10.0);
        ui.add_space(10.0);
        section(ui, "登录凭证快照");
        // 当前登录账号（head_imgs/0 下永远只有一个文件，文件名 = 账号数字 ID）
        let cur = crate::wechatsnap::current_account();
        ui.horizontal(|ui| {
            ui.label(row_label("当前登录"));
            match cur {
                Some((id, _)) => ui.label(
                    egui::RichText::new(format!("账号 {id}"))
                        .size(12.0)
                        .color(ok_green()),
                ),
                None => ui.label(
                    egui::RichText::new("检测不到（微信未登录或数据目录不对）")
                        .size(12.0)
                        .weak(),
                ),
            };
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.label(row_label("凭证昵称"));
            ui.add_sized(
                [240.0, 24.0],
                egui::TextEdit::singleline(&mut self.edits.snap_name)
                    .hint_text("给这个账号的凭证起个名字，例：办执照"),
            );
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("保存当前凭证").size(12.0))
                        .min_size(egui::vec2(104.0, 24.0)),
                )
                .clicked()
            {
                let name = self.edits.snap_name.trim().to_string();
                if name.is_empty() {
                    self.snap_status = "先填凭证昵称再保存".into();
                } else {
                    self.snap_status = match crate::wechatsnap::save(&name) {
                        Ok(s) => format!("已保存「{}」（账号 {}）", s.nickname, s.id),
                        Err(e) => format!("保存失败：{e}"),
                    };
                    self.edits.snap_name.clear();
                }
            }
        });
        ui.label(
            egui::RichText::new(
                "保存的是当前登录状态的凭证（global/client_config 四件套 + 头像）。微信登录成功后随时可保存，不必退出",
            )
            .size(11.5)
            .weak(),
        );

        // 快照列表：昵称（账号 ID）+ 启动 / 创建快捷方式 / 删除
        let snaps = crate::wechatsnap::list();
        if !snaps.is_empty() {
            ui.add_space(6.0);
            for s in &snaps {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(egui::RichText::new(s.nickname.as_str()).size(12.5));
                    ui.label(egui::RichText::new(format!("（账号 {}）", s.id)).size(11.0).weak());
                    // 登录状态：account.txt 里记录的 pid 还活着且是 Weixin.exe → 已登录
                    match crate::wechatsnap::logged_in(&s.id) {
                        Some(pid) => ui.label(
                            egui::RichText::new(format!("已登录（pid {pid}）"))
                                .size(12.0)
                                .color(ok_green()),
                        ),
                        None => ui.label(
                            egui::RichText::new("未登录").size(12.0).weak(),
                        ),
                    };
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("启动").size(12.0))
                                .min_size(egui::vec2(64.0, 24.0)),
                        )
                        .clicked()
                    {
                        self.snap_status = match crate::wechatsnap::launch(&s.id) {
                            Ok(()) => format!("已恢复「{}」凭证并启动微信", s.nickname),
                            Err(e) => format!("启动失败：{e}"),
                        };
                    }
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("创建快捷方式").size(12.0))
                                .min_size(egui::vec2(104.0, 24.0)),
                        )
                        .clicked()
                    {
                        self.snap_status = match crate::wechatsnap::make_shortcut(&s.id) {
                            Ok(p) => format!("已创建桌面快捷方式：{}", p.display()),
                            Err(e) => format!("创建快捷方式失败：{e}"),
                        };
                    }
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("删除").size(12.0))
                                .min_size(egui::vec2(56.0, 24.0)),
                        )
                        .clicked()
                    {
                        self.snap_status = match crate::wechatsnap::delete(&s.id) {
                            Ok(()) => format!("已删除「{}」", s.nickname),
                            Err(e) => format!("删除失败：{e}"),
                        };
                    }
                });
            }
            ui.label(
                egui::RichText::new(
                    "「启动」= 恢复该快照凭证（热替换，运行中的微信不受影响）→ 自动打开微信免扫码登录，并把本次拉起的微信 pid 记进 account.txt（据此显示登录状态，pid 退出即变回未登录）。多开：先开 A 号，再点 B 号快照的「启动」",
                )
                .size(11.5)
                .weak(),
            );
        }
        if !self.snap_status.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(&self.snap_status).size(12.0).color(ok_green()));
        }
        ui.label(
            egui::RichText::new(
                "原理：微信把「当前登录账号」存在数据目录 all_users/config/ 的 global/client_config 里；快照备份该组文件，恢复时用改名法绕过运行中微信的文件锁（只挪开不关闭句柄，对微信无感）。桌面快捷方式启动 = 恢复凭证 + 拉起微信，可多号共存",
            )
            .size(11.0)
            .weak(),
        );
    }

    fn detail_pill(&mut self, ui: &mut egui::Ui, ver_changed: bool) {
        section(ui, "顶部胶囊");
        ui.label(
            egui::RichText::new("透明度、大小、形状、圆角和 Dock 位置都在「常规 → 外观」里调，这里只管胶囊内容")
                .size(11.5)
                .weak(),
        );
        ui.add_space(6.0);
        egui::Grid::new("g_pill")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label(row_label("时钟格式"));
                ui.vertical(|ui| {
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
                    // 占位符对照（对应 clock::format_time 支持的全部占位符）
                    ui.label(
                        egui::RichText::new(
                            "%Y 年   %m 月   %d 日   %H 时   %M 分   %S 秒",
                        )
                        .size(10.5)
                        .weak(),
                    );
                    ui.label(
                        egui::RichText::new(
                            "%y 两位年   %I 12时制   %p 上午/下午   %a %A 周几   %% 百分号",
                        )
                        .size(10.5)
                        .weak(),
                    );
                });
                ui.end_row();

                ui.label(row_label("实时预览"));
                ui.label(
                    egui::RichText::new(clock::sample(&self.edits.clock))
                        .size(13.0)
                        .color(argb(cfg::cur().accent)),
                );
                ui.end_row();
            });

        ui.add_space(10.0);
        section(ui, "时间与音乐条");
        egui::Grid::new("g_pill_music")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label(row_label("音乐条位置"));
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

                let lead = settings::lyric_lead();
                ui.label(row_label("歌词提前量"));
                if let Some(d) = stepper(
                    ui,
                    &format!("{lead:+.1}s"),
                    lead > -settings::LYRIC_LEAD_MAX + 0.05,
                    lead < settings::LYRIC_LEAD_MAX - 0.05,
                ) {
                    // 0.1 秒一步；负值 = 歌词延后（提前量可以为负）
                    settings::set_lyric_lead(lead + d as f32 * 0.1);
                }
                ui.end_row();
            });
        ui.label(
            egui::RichText::new("歌词字号默认与时间日期字号一致（20）")
                .size(11.5)
                .weak(),
        );

        ui.add_space(10.0);
        section(ui, "模块排序");
        self.chips(ui);
        ui.label(
            egui::RichText::new("下拉选择位置：胶囊 / 左条 / 右条 / 隐藏；▲▼ 调整顺序")
                .size(11.5)
                .weak(),
        );
    }

    /// 功能模块：天气卡片 / 剪切板 / 邮件——Orbit 的独立附加功能模块，
    /// 各自有独立面板或卡片，不再归在「顶部胶囊」下。
    fn detail_modules(&mut self, ui: &mut egui::Ui, ver_changed: bool) {
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

                // 天气卡片配色两套：深色背景一套（浅色字）、浅色背景一套（深色字）；
                // 默认「自动跟壁纸」保持原来的自动明暗切换。切换后立即通知卡片重绘
                ui.label(row_label("天气配色"));
                let sch = settings::weather_scheme();
                if let Some(i) = seg(ui, &["自动跟壁纸", "深色背景", "浅色背景"], sch.min(2) as usize)
                {
                    settings::set_weather_scheme(i as u32);
                    // 卡片窗口在主进程：立即按新配色重绘，不等 500ms 轮询
                    post_to_parent(crate::appmsg::WM_SETTINGS_APPLIED);
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
        ui.label(
            egui::RichText::new("天气配色：「深色背景」用浅色字、「浅色背景」用深色字，各一套配色；「自动跟壁纸」按卡片周围壁纸明暗自动切换")
                .size(11.5)
                .weak(),
        );

        ui.add_space(10.0);
        section(ui, "剪切板");
        egui::Grid::new("g_clip")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label(row_label("显示"));
                let mode = settings::module_mode("clipboard");
                let idx = match mode {
                    settings::ModuleMode::Pill => 0,
                    settings::ModuleMode::BarLeft => 1,
                    settings::ModuleMode::BarRight => 2,
                    settings::ModuleMode::Off => 3,
                };
                if let Some(i) = seg(ui, &["胶囊", "左条", "右条", "隐藏"], idx) {
                    let m = match i {
                        1 => settings::ModuleMode::BarLeft,
                        2 => settings::ModuleMode::BarRight,
                        3 => settings::ModuleMode::Off,
                        _ => settings::ModuleMode::Pill,
                    };
                    settings::set_module_mode("clipboard", m);
                }
                ui.end_row();
            });
        ui.horizontal(|ui| {
            ui.label(row_label("常驻文字"));
            let r = ui.add_sized(
                [CTRL_W, 24.0],
                egui::TextEdit::singleline(&mut self.edits.clipboard_pinned)
                    .hint_text("固定显示在剪切板面板顶部，留空关闭"),
            );
            if r.lost_focus() && self.edits.clipboard_pinned != settings::clipboard_pinned_text() {
                settings::set_clipboard_pinned_text(self.edits.clipboard_pinned.clone());
                crate::tlog!("[settings] 剪切板常驻文字已更新");
            }
            if ver_changed && !r.has_focus() {
                self.edits.clipboard_pinned = settings::clipboard_pinned_text();
            }
        });

        ui.add_space(10.0);
        self.mail_accounts_ui(ui, ver_changed);
    }

    /// 邮件：多账号管理（添加 / 编辑 / 移除 / 测试连通 / 连通标记持久化）
    fn mail_accounts_ui(&mut self, ui: &mut egui::Ui, ver_changed: bool) {
        section(ui, "邮件");
        ui.label(
            egui::RichText::new("可保存多个邮箱账号同时监听；连通测试通过后会打上 ✓ 标记并记住，不用每次重测")
                .size(11.5)
                .weak(),
        );

        let accounts = settings::mail_accounts();
        let editing = self.mail_edit;

        if let Some(idx) = editing {
            // ---- 编辑某个账号 ----
            // 服务器预设
            let custom = !MAIL_PRESETS.iter().any(|(_, h, _)| *h == self.edits.host);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (name, host, port) in MAIL_PRESETS {
                    let sel = self.edits.host == host;
                    if ui
                        .add(egui::Button::selectable(
                            sel,
                            egui::RichText::new(name).size(12.5).color(if sel {
                                ink_on(cfg::cur().accent)
                            } else {
                                argb(cfg::cur().text_body)
                            }),
                        ))
                        .clicked()
                    {
                        self.edits.host = host.to_string();
                        self.edits.port = port.to_string();
                    }
                }
                if ui
                    .add(egui::Button::selectable(
                        custom,
                        egui::RichText::new("自定义").size(12.5).color(if custom {
                            ink_on(cfg::cur().accent)
                        } else {
                            argb(cfg::cur().text_body)
                        }),
                    ))
                    .clicked()
                {
                    if !custom {
                        self.edits.host.clear();
                        self.edits.port = cfg::MAIL_PORT.to_string();
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
                            egui::TextEdit::singleline(&mut self.edits.host)
                                .hint_text("imap.example.com"),
                        );
                        if r.lost_focus() {
                            self.edits.port = self
                                .edits
                                .port
                                .trim()
                                .parse::<u16>()
                                .map(|_| self.edits.port.clone())
                                .unwrap_or(cfg::MAIL_PORT.to_string());
                        }
                        ui.end_row();

                        ui.label(row_label("端口"));
                        ui.add_sized([CTRL_W, 24.0], egui::TextEdit::singleline(&mut self.edits.port));
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
            egui::Grid::new("g_mail_acc")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label(row_label("账号"));
                    ui.add_sized([CTRL_W, 24.0], egui::TextEdit::singleline(&mut self.edits.user));
                    ui.end_row();

                    ui.label(row_label("授权码"));
                    ui.add_sized(
                        [CTRL_W, 24.0],
                        egui::TextEdit::singleline(&mut self.edits.pass).password(true),
                    );
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
                if ui.add(egui::Button::new("保存").small()).clicked() {
                    let mut list = settings::mail_accounts();
                    let new_acc = settings::MailAccount {
                        host: self.edits.host.trim().to_string(),
                        port,
                        user: self.edits.user.trim().to_string(),
                        pass: self.edits.pass.clone(),
                    };
                    if idx < list.len() {
                        list[idx] = new_acc;
                    } else {
                        list.push(new_acc);
                    }
                    settings::set_mail_accounts(list);
                    self.mail_edit = None;
                    crate::tlog!("[settings] 邮箱账号已保存：{}", self.edits.user);
                }
                if ui.add(egui::Button::new("取消").small()).clicked() {
                    self.mail_edit = None;
                }
                let (h, u, w) =
                    (self.edits.host.clone(), self.edits.user.clone(), self.edits.pass.clone());
                let running = matches!(st, TestState::Running);
                if ui.add_enabled(!running, egui::Button::new("测试连通").small()).clicked() {
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
                        let acc = settings::MailAccount {
                            host: h,
                            port,
                            user: u,
                            pass: w,
                        };
                        std::thread::spawn(move || {
                            let ident = acc.ident();
                            let res = crate::mail::test_account(&(acc.host.clone(), acc.port, acc.user.clone(), acc.pass.clone()));
                            let ok = res.is_ok();
                            // 连通成功就打上 ✓ 标记并持久化，下次不用重测
                            settings::set_mail_verified(&ident, ok);
                            if let Ok(mut s) = shared.lock() {
                                *s = match res {
                                    Ok(_) => TestState::Ok("连接成功，已标记为已连通".to_string()),
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
        } else {
            // ---- 账号列表 ----
            if accounts.is_empty() {
                ui.label(
                    egui::RichText::new("还没有邮箱账号，点「添加账号」开始")
                        .size(11.5)
                        .weak(),
                );
            }
            for (i, a) in accounts.iter().enumerate() {
                let verified = settings::is_mail_verified(&a.ident());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(egui::RichText::new(format!("{}. {}", i + 1, a.user)).size(12.5));
                    ui.label(
                        egui::RichText::new(format!("{}:{}", a.host, a.port))
                            .size(11.0)
                            .color(argb(cfg::cur().text_dim)),
                    );
                    ui.label(
                        egui::RichText::new(if verified { "✓ 已连通" } else { "未验证" })
                            .size(11.5)
                            .color(if verified {
                                ok_green()
                            } else {
                                argb(cfg::cur().text_dim)
                            }),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new("移除").small()).clicked() {
                            let mut list = settings::mail_accounts();
                            let ident = list[i].ident();
                            list.remove(i);
                            settings::set_mail_accounts(list);
                            settings::set_mail_verified(&ident, false);
                        }
                        if ui.add(egui::Button::new("编辑").small()).clicked() {
                            self.edits.host = a.host.clone();
                            self.edits.port = a.port.to_string();
                            self.edits.user = a.user.clone();
                            self.edits.pass = a.pass.clone();
                            if let Ok(mut s) = self.test.lock() {
                                *s = TestState::Idle;
                            }
                            self.mail_edit = Some(i);
                        }
                    });
                });
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("添加账号").small()).clicked() {
                    self.edits.host = MAIL_PRESETS[0].1.to_string();
                    self.edits.port = MAIL_PRESETS[0].2.to_string();
                    self.edits.user.clear();
                    self.edits.pass.clear();
                    if let Ok(mut s) = self.test.lock() {
                        *s = TestState::Idle;
                    }
                    self.mail_edit = Some(accounts.len());
                }
            });
        }
        let _ = (accounts.len(), ver_changed);
    }

    fn detail_dock(&mut self, ui: &mut egui::Ui) {
        section(ui, "Dock");

        // 内建「系统控制」按钮开关（默认隐藏）
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let on = settings::sys_panel();
            if toggle_btn(ui, on, "系统控制") {
                settings::set_sys_panel(!on);
                crate::tlog!(
                    "[settings] 系统控制入口 → {}",
                    if settings::sys_panel() { "显示" } else { "隐藏" }
                );
            }
            ui.label(
                egui::RichText::new("Dock 上的内建按钮：音量 / 亮度 / WiFi / 蓝牙（Orbit 图标）")
                    .size(11.5)
                    .weak(),
            );
        });
        ui.add_space(6.0);
        self.tray_list(ui);
        ui.add_space(10.0);
        self.pins(ui);
    }

    /// 托盘图标统一列表：一张表里同时完成 显示/隐藏、图标指定、拖动排序，
    /// 不再分「托盘图标开关」「图标指定」两张表。
    /// 顺序：自定义排序（`tray_order`）优先，没进表的图标按快照顺序跟在后面；
    /// 没有自定义排序时就是 Dock 的默认顺序。
    fn tray_list(&mut self, ui: &mut egui::Ui) {
        // 右区宽度 = 图标指定(84) + 显示/隐藏(56) + 间距(6) + 8(余量)
        const RIGHT_W: f32 = 84.0 + 56.0 + 6.0 + 8.0;
        section(ui, "托盘图标");
        ui.label(
            egui::RichText::new(
                "按住行首圆点拖动调整在 Dock 上的排列顺序；按钮切换 显示/隐藏\
                 （隐藏的会转发给系统托盘）；「指定图标…」给发糊的小图标换高清来源\
                 （exe / dll / ico / png）。「返回桌面」固定在第一行，不参与排序",
            )
            .size(11.5)
            .weak(),
        );

        // 返回桌面细长条：表格第一行，位置固定不可拖动（无把手）。
        // 列口径与下面各行一致：无把手占位对齐名称列，空占位对齐「图标指定」列
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.allocate_exact_size(egui::vec2(14.0, 18.0), egui::Sense::hover());
            let name_w = (ui.available_width() - RIGHT_W).max(80.0);
            ui.allocate_ui_with_layout(
                egui::vec2(name_w, 22.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_max_width(name_w);
                    ui.add(egui::Label::new(egui::RichText::new("返回桌面").size(12.5)));
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                // 空占位（84 宽）：与下面各行的「图标指定」按钮列对齐
                ui.allocate_exact_size(egui::vec2(84.0, 22.0), egui::Sense::hover());
                let on = settings::show_desktop_btn();
                let fill = if on { on_fill() } else { off_fill() };
                let txt = egui::RichText::new(if on { "显示" } else { "隐藏" })
                    .size(12.0)
                    .color(if on { on_text() } else { off_text() });
                if ui
                    .add(egui::Button::new(txt).fill(fill).min_size(egui::vec2(56.0, 22.0)))
                    .clicked()
                {
                    settings::set_show_desktop_btn(!on);
                    crate::tlog!(
                        "[settings] 返回桌面细长条 → {}",
                        if settings::show_desktop_btn() { "显示" } else { "隐藏" }
                    );
                }
            });
        });

        // 自定义排序优先：表里的键按其下标排，没进表的按快照顺序跟在后面（稳定排序）
        let mut entries = tray_entries();
        let order = settings::tray_order();
        entries.sort_by_key(|(k, _)| order.iter().position(|o| o == k).unwrap_or(usize::MAX));
        let overs = settings::icon_overrides();

        // 表格式三列布局：[拖动把手] [名称靠左] … [显示/隐藏] [图标指定]（后两列靠右）。
        // 右侧两个按钮全部固定宽度，保证各行的列严格对齐（同开始菜单快捷方式的行）。
        let dragging = egui::DragAndDrop::payload::<TrayDrag>(ui.ctx()).map(|p| p.0);
        let mut row_rects: Vec<(usize, egui::Rect)> = Vec::new();
        for (idx, (key, name)) in entries.iter().enumerate() {
            let hidden = settings::tray_hidden(key);
            let over = overs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, p)| p.clone());
            let row = ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                // 拖动把手：2×3 圆点 grip（手绘，不依赖字体字形）。按住拖到目标行即可换位
                let (hr, hresp) = ui.allocate_exact_size(egui::vec2(14.0, 18.0), egui::Sense::drag());
                let dot_color = if dragging == Some(idx) {
                    argb(cfg::cur().accent)
                } else {
                    argb(cfg::cur().text_dim)
                };
                let c = hr.center();
                for dx in [-3.0f32, 3.0] {
                    for dy in [-4.5f32, 0.0, 4.5] {
                        ui.painter().circle_filled(c + egui::vec2(dx, dy), 1.2, dot_color);
                    }
                }
                let hresp = hresp.on_hover_text("按住拖动调整顺序");
                if hresp.drag_started() {
                    egui::DragAndDrop::set_payload(ui.ctx(), TrayDrag(idx));
                }
                if hresp.drag_stopped() {
                    egui::DragAndDrop::clear_payload(ui.ctx());
                }
                hresp.on_hover_cursor(egui::CursorIcon::Grab);

                // 名称列靠左：固定宽度 + truncate（长名字在本列截断，绝不把行撑宽）。
                // add_sized 内部固定用居中布局，所以自己用左对齐布局分配矩形
                let mut label = name.clone();
                if let Some(p) = &over {
                    label.push_str(&format!("  → {}", pin_label(p)));
                }
                let name_w = (ui.available_width() - RIGHT_W).max(80.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(name_w, 22.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_max_width(name_w);
                        ui.add(egui::Label::new(egui::RichText::new(label).size(12.5)).truncate());
                    },
                );

                // 右区：right_to_left 布局从右缘往左摆按钮，先加的在最右。
                // 左→右的列顺序 = 显示/隐藏、图标指定，所以先加「图标指定」再加开关
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    // 图标指定：已指定的显示「还原」，没指定的显示「指定图标…」
                    // （固定 84 宽，足够放下「指定图标…」不撑开）
                    let icon_btn = |ui: &mut egui::Ui, t: &str| {
                        ui.add(
                            egui::Button::new(egui::RichText::new(t).size(11.5))
                                .min_size(egui::vec2(84.0, 22.0)),
                        )
                        .clicked()
                    };
                    if over.is_some() {
                        if icon_btn(ui, "还原") {
                            settings::set_icon_override(key, None);
                            crate::tlog!("[settings] 托盘图标 {key} 图标指定已还原");
                        }
                    } else if icon_btn(ui, "指定图标…") {
                        if let Some(path) = pick_program(true) {
                            settings::set_icon_override(key, Some(&path));
                            crate::tlog!("[settings] 托盘图标 {key} 指定图标源 → {path}");
                        }
                    }
                    // 显示 / 隐藏（固定 56 宽）
                    let fill = if hidden { off_fill() } else { on_fill() };
                    let txt = egui::RichText::new(if hidden { "隐藏" } else { "显示" })
                        .size(12.0)
                        .color(if hidden { off_text() } else { on_text() });
                    if ui
                        .add(egui::Button::new(txt).fill(fill).min_size(egui::vec2(56.0, 22.0)))
                        .clicked()
                    {
                        settings::set_tray_hidden(key, !hidden);
                        crate::tlog!(
                            "[settings] 托盘图标 {key} → {}",
                            if !hidden { "隐藏" } else { "显示" }
                        );
                    }
                });
            });
            row_rects.push((idx, row.response.rect));
        }

        // 拖着走的行悬到另一行上：实时交换顺序（松手即定格），写回自定义排序表
        if let Some(src) = dragging {
            if let Some(pos) = ui.ctx().input(|i| i.pointer.latest_pos()) {
                if let Some(&(j, _)) = row_rects.iter().find(|(_, r)| r.contains(pos)) {
                    if j != src {
                        let mut keys: Vec<String> =
                            entries.iter().map(|(k, _)| k.clone()).collect();
                        if src < keys.len() && j < keys.len() {
                            let item = keys.remove(src);
                            keys.insert(j, item);
                            settings::set_tray_order(keys);
                            egui::DragAndDrop::set_payload(ui.ctx(), j);
                        }
                    }
                }
            }
        }

        let has_hidden = !settings::hidden_tray().is_empty();
        let has_order = !settings::tray_order().is_empty();
        if has_hidden || has_order {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if has_hidden && ui.add(egui::Button::new("恢复全部显示").small()).clicked() {
                    settings::clear_hidden_tray();
                    crate::tlog!("[settings] 托盘隐藏列表清空");
                }
                if has_order && ui.add(egui::Button::new("恢复默认排序").small()).clicked() {
                    settings::set_tray_order(Vec::new());
                    crate::tlog!("[settings] 托盘自定义排序已清空（回到默认顺序）");
                }
            });
        }
    }

    /// 固定图标：一张表 = 已固定（行首把手可拖动排序）+ 正在运行但未固定（无把手、
    /// 不可拖动）。右侧统一用开关按钮切换 固定/未固定（与托盘列表的 显示/隐藏 同款）。
    /// 下面保留「浏览程序…」添加没在运行的程序。所有改动整体写回列表。
    fn pins(&mut self, ui: &mut egui::Ui) {
        section(ui, "固定图标");
        ui.label(
            egui::RichText::new(
                "已固定的可按住行首圆点拖动排序；点右侧按钮取消固定。\
                 未固定的行是正在运行的应用，点「固定」加到 Dock",
            )
            .size(11.5)
            .weak(),
        );
        let (pins, running) = pins_view();

        // 拖动排序：payload 用 PinDrag 类型，与托盘列表的 TrayDrag 区分（两表同屏）
        let dragging = egui::DragAndDrop::payload::<PinDrag>(ui.ctx()).map(|p| p.0);
        let mut row_rects: Vec<(usize, egui::Rect)> = Vec::new();

        if pins.is_empty() {
            ui.label(
                egui::RichText::new("还没有固定图标：下面的运行中应用点「固定」即可添加")
                    .size(11.5)
                    .weak(),
            );
        }

        // 已固定的行（按固定顺序，行首把手可拖动）
        for (idx, path) in pins.iter().enumerate() {
            let name = pin_label(path);
            let row = ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                // 拖动把手：2×3 圆点 grip（手绘，不依赖字体字形）
                let (hr, hresp) =
                    ui.allocate_exact_size(egui::vec2(14.0, 18.0), egui::Sense::drag());
                let dot_color = if dragging == Some(idx) {
                    argb(cfg::cur().accent)
                } else {
                    argb(cfg::cur().text_dim)
                };
                let c = hr.center();
                for dx in [-3.0f32, 3.0] {
                    for dy in [-4.5f32, 0.0, 4.5] {
                        ui.painter().circle_filled(c + egui::vec2(dx, dy), 1.2, dot_color);
                    }
                }
                let hresp = hresp.on_hover_text("按住拖动调整顺序");
                if hresp.drag_started() {
                    egui::DragAndDrop::set_payload(ui.ctx(), PinDrag(idx));
                }
                if hresp.drag_stopped() {
                    egui::DragAndDrop::clear_payload(ui.ctx());
                }
                hresp.on_hover_cursor(egui::CursorIcon::Grab);
                // 名称列靠左（固定宽度 + truncate，与托盘列表同一套做法）
                let name_w = (ui.available_width() - PIN_RIGHT).max(80.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(name_w, 22.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_max_width(name_w);
                        ui.add(egui::Label::new(egui::RichText::new(&name).size(12.5)).truncate());
                    },
                );
                // 右侧开关（与托盘列表 显示/隐藏 同款按钮）
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let fill = on_fill();
                    let txt = egui::RichText::new("已固定")
                        .size(12.0)
                        .color(on_text());
                    if ui
                        .add(egui::Button::new(txt).fill(fill).min_size(egui::vec2(PIN_BTN, 22.0)))
                        .clicked()
                    {
                        let list: Vec<String> = pins
                            .iter()
                            .filter(|p| !p.eq_ignore_ascii_case(path))
                            .cloned()
                            .collect();
                        settings::set_dock_pins(list);
                        pins_cache_clear();
                        crate::tlog!("[settings] 固定图标 取消固定 {name}");
                    }
                });
            });
            row_rects.push((idx, row.response.rect));
        }

        // 正在运行但未固定的行：无把手、不可拖动，名称用次级色区分
        let unpinned: Vec<(String, String)> = running
            .into_iter()
            .filter(|(_, exe)| !pins.iter().any(|p| p.eq_ignore_ascii_case(exe)))
            .collect();
        if unpinned.is_empty() && !pins.is_empty() {
            ui.label(
                egui::RichText::new("（运行中的应用都已固定）").size(11.5).weak(),
            );
        }
        for (name, exe) in &unpinned {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                // 无把手：空占位，让名称列与上面已固定的行对齐
                ui.allocate_exact_size(egui::vec2(14.0, 18.0), egui::Sense::hover());
                let name_w = (ui.available_width() - PIN_RIGHT).max(80.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(name_w, 22.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_max_width(name_w);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(name)
                                    .size(12.5)
                                    .color(argb(cfg::cur().text_dim)),
                            )
                            .truncate(),
                        );
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let fill = off_fill();
                    let txt = egui::RichText::new("固定")
                        .size(12.0)
                        .color(off_text());
                    if ui
                        .add(egui::Button::new(txt).fill(fill).min_size(egui::vec2(PIN_BTN, 22.0)))
                        .clicked()
                    {
                        let mut list = pins.clone();
                        list.push(exe.clone());
                        settings::set_dock_pins(list);
                        pins_cache_clear();
                        crate::tlog!("[settings] 固定图标 已固定 {name}");
                    }
                });
            });
        }

        // 拖着走的行悬到另一行（仅已固定行）上：实时交换顺序（松手即定格）
        if let Some(src) = dragging {
            if let Some(pos) = ui.ctx().input(|i| i.pointer.latest_pos()) {
                if let Some(&(j, _)) = row_rects.iter().find(|(_, r)| r.contains(pos)) {
                    if j != src {
                        let mut list = pins.clone();
                        if src < list.len() && j < list.len() {
                            let item = list.remove(src);
                            list.insert(j, item);
                            settings::set_dock_pins(list);
                            pins_cache_clear();
                            egui::DragAndDrop::set_payload(ui.ctx(), PinDrag(j));
                        }
                    }
                }
            }
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new("浏览程序…").small()).clicked() {
                if let Some(path) = pick_program(false) {
                    let mut list = pins.clone();
                    if !list.iter().any(|p| p.eq_ignore_ascii_case(&path)) {
                        list.push(path);
                        settings::set_dock_pins(list);
                        pins_cache_clear();
                    }
                }
            }
            if !pins.is_empty() && ui.add(egui::Button::new("清空固定").small()).clicked() {
                settings::set_dock_pins(Vec::new());
                pins_cache_clear();
            }
        });
    }


    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // 没有系统边框，标题栏要自己承担"拖动窗口"：
        // 在标题栏左段（右侧 ~150px 留给「应用 / 还原 / ✕」按钮）铺一个拖动响应，
        // 按下去就发 `StartDrag`，交给系统进入移动循环——和拖原生标题栏一个手感
        // （含贴边吸附）。刻意避开按钮区：压在按钮上按下再拖不会变成拖窗口。
        let bar = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2((ui.available_width() - 150.0).max(60.0), 28.0),
        );
        let drag = ui.interact(bar, ui.make_persistent_id("set_titlebar"), egui::Sense::drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        drag.on_hover_cursor(egui::CursorIcon::Grab);

        ui.horizontal(|ui| {
            ui.label(egui_soft::bold("Orbit 设置").size(17.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if close_button(ui).clicked() {
                    self.do_hide(ctx);
                }
                // 「还原」：丢弃本次编辑（未应用时才有意义）
                ui.add_enabled_ui(settings::editing(), |ui| {
                    if ui.add(egui::Button::new("还原").small()).clicked() {
                        settings::discard_edit();
                        self.load_edits();
                        crate::appmsg::notify_applied();
                        post_to_parent(crate::appmsg::WM_SETTINGS_APPLIED);
                        crate::tlog!("[settings] 已还原未应用的改动");
                    }
                });
                // 「应用」：编辑会话中才生效，点击统一落盘
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("应用")
                                .size(12.0)
                                .color(if settings::editing() { on_text() } else { off_text() }),
                        )
                        .fill(if settings::editing() { on_fill() } else { off_fill() })
                        .min_size(egui::vec2(52.0, 24.0)),
                    )
                    .clicked()
                {
                    settings::commit_edit();
                    // 主进程（胶囊/Dock/天气卡片）收到就立刻重绘，不用等 500ms 轮询
                    post_to_parent(crate::appmsg::WM_SETTINGS_APPLIED);
                    crate::tlog!("[settings] 已应用改动");
                }
            });
        });
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(10.0);
    }

    /// 模块位置/隐藏：每模块一行，位置下拉 + 上下移按钮（替代原来的「点击芯片循环 + 拖动换位」）。
    fn chips(&mut self, ui: &mut egui::Ui) {
        // 位置选项：胶囊 / 左条 / 右条 / 隐藏，与 ModuleMode 一一对应
        const POSITIONS: [(&str, ModuleMode); 4] = [
            ("胶囊", ModuleMode::Pill),
            ("左条", ModuleMode::BarLeft),
            ("右条", ModuleMode::BarRight),
            ("隐藏", ModuleMode::Off),
        ];

        let order = settings::module_order();
        let n = order.len();
        for (idx, name) in order.iter().enumerate() {
            let m = settings::module_mode(name);
            // 下拉当前选中项的显示名
            let cur_label = POSITIONS
                .iter()
                .find(|(_, mm)| *mm == m)
                .map(|(l, _)| *l)
                .unwrap_or("胶囊");

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                // 模块名（固定宽度对齐），邮件胶囊模式补一句说明
                let mut label = module_label(name).to_string();
                if name == "mail" && m == ModuleMode::Pill {
                    label.push_str("（有新邮件时）");
                }
                ui.add_sized(
                    [64.0, 22.0],
                    egui::Label::new(egui::RichText::new(label).size(12.5)),
                );

                if name == "weather" {
                    // 天气已移出胶囊，改为桌面右上角常驻卡片：没有 胶囊/左条/右条
                    // 可选，只留一个 显示/隐藏 开关
                    let off = m == ModuleMode::Off;
                    if let Some(i) = seg(ui, &["显示", "隐藏"], if off { 1 } else { 0 }) {
                        settings::set_module_mode(
                            name,
                            if i == 1 { ModuleMode::Off } else { ModuleMode::Pill },
                        );
                        crate::tlog!(
                            "[settings] 天气卡片 → {}",
                            if i == 1 { "隐藏" } else { "显示" }
                        );
                    }
                    ui.label(
                        egui::RichText::new("常驻桌面右上角").size(11.5).weak(),
                    );
                    return; // 天气不参与胶囊排序，不需要 上下移
                }

                // 位置下拉
                egui::ComboBox::from_id_salt(format!("mod_pos_{name}"))
                    .width(92.0)
                    .selected_text(cur_label)
                    .show_ui(ui, |ui| {
                        for (lbl, mm) in POSITIONS {
                            if ui
                                .selectable_label(m == mm, egui::RichText::new(lbl).size(12.5))
                                .clicked()
                            {
                                settings::set_module_mode(name, mm);
                                crate::tlog!("[settings] 模块 {name} → {mm:?}");
                            }
                        }
                    });

                // 上下移（第一项不能上移，最后一项不能下移）
                ui.add_enabled_ui(idx > 0, |ui| {
                    if ui.add(egui::Button::new("▲").small()).clicked() {
                        let mut o = settings::module_order();
                        o.swap(idx, idx - 1);
                        settings::set_module_order(o);
                        crate::tlog!("[settings] 模块顺序 → {:?}", settings::module_order());
                    }
                });
                ui.add_enabled_ui(idx + 1 < n, |ui| {
                    if ui.add(egui::Button::new("▼").small()).clicked() {
                        let mut o = settings::module_order();
                        o.swap(idx, idx + 1);
                        settings::set_module_order(o);
                        crate::tlog!("[settings] 模块顺序 → {:?}", settings::module_order());
                    }
                });
            });
        }

        // 时间 · 日期固定在最前，不参与排序
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_sized([64.0, 22.0], egui::Label::new(egui::RichText::new("时间 · 日期").size(12.5)));
            ui.label(
                egui::RichText::new("固定显示在胶囊最前").size(11.5).weak(),
            );
        });
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

/// 开/关按钮：返回 true 表示这次点击把它切成了另一个状态
fn toggle_btn(ui: &mut egui::Ui, on: bool, label: &str) -> bool {
    let fill = if on { on_fill() } else { off_fill() };
    let txt = egui::RichText::new(if on { "已开启" } else { "已关闭" })
        .size(12.0)
        .color(if on { on_text() } else { off_text() });
    let r = ui.add(egui::Button::new(txt).fill(fill).min_size(egui::vec2(76.0, 24.0)));
    r.clicked() && { let _ = label; true }
}

fn section(ui: &mut egui::Ui, title: &str) {
    // 段标题与上下内容（分割线 / 上一组控件）之间留出间隙，别贴着线。
    // 标题本身用真正的粗体族，不再靠 `.weak()` / `.strong()` 只改颜色。
    ui.add_space(12.0);
    ui.label(
        egui_soft::bold(title)
            .size(12.5)
            .color(argb(cfg::cur().text_body)),
    );
    ui.add_space(8.0);
}

fn row_label(s: &str) -> egui::RichText {
    egui_soft::bold(s).size(13.0)
}

/// 分段选择：返回被点中的段（None = 没点）
fn seg(ui: &mut egui::Ui, options: &[&str], active: usize) -> Option<usize> {
    let mut pick = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, o) in options.iter().enumerate() {
            let btn = egui::Button::selectable(
                i == active,
                egui::RichText::new(*o).size(12.0).color(if i == active {
                    ink_on(cfg::cur().accent)
                } else {
                    argb(cfg::cur().text_body)
                }),
            );
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
/// 这里按 2 秒缓存一份；表内改动（固定/取消固定/拖动排序）走 `pins_cache_clear()`
/// 立刻清缓存，下一帧就能看到变化，不用等 2 秒自然过期。
static PINS_CACHE: Mutex<Option<(std::time::Instant, Vec<String>, Vec<(String, String)>)>> =
    Mutex::new(None);

/// 表内改动后立刻清固定列表缓存（下一帧重扫，改动马上可见）
fn pins_cache_clear() {
    if let Ok(mut c) = PINS_CACHE.lock() {
        *c = None;
    }
}

fn pins_view() -> (Vec<String>, Vec<(String, String)>) {
    if let Ok(c) = PINS_CACHE.lock() {
        if let Some((at, pins, running)) = c.as_ref() {
            if at.elapsed().as_millis() < 2000 {
                return (pins.clone(), running.clone());
            }
        }
    }
    let pins = crate::dock::effective_pins();
    let running = crate::dock::running_app_list();
    if let Ok(mut c) = PINS_CACHE.lock() {
        *c = Some((std::time::Instant::now(), pins.clone(), running.clone()));
    }
    (pins, running)
}

/// 拖动排序的 payload 类型：托盘列表与固定图标列表在 Dock 页同屏可见，
/// 必须各用一个独立类型，否则共用 `usize` 时 A 表拖动会误触发 B 表换位
struct TrayDrag(usize);
struct PinDrag(usize);

/// 「固定图标」行右侧开关按钮的宽度（放下三个字「已固定/未固定」）
const PIN_BTN: f32 = 68.0;
/// 固定图标行的右区预算（按钮 + 间距 + 余量），名称列据此留位
const PIN_RIGHT: f32 = PIN_BTN + 10.0;

/// 固定列表里显示的名字：去掉目录和扩展名
fn pin_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string())
}

/// 挑一个文件：`for_icon` = 图标源（exe / dll / ico 都行），否则只要程序（exe / 快捷方式）
fn pick_program(for_icon: bool) -> Option<String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OPENFILENAMEW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST,
    };
    let mut buf = vec![0u16; 1024];
    let spec = if for_icon {
        "程序或图标 (*.exe;*.dll;*.ico;*.png;*.jpg)\0*.exe;*.dll;*.ico;*.png;*.jpg;*.bmp\0图标文件 (*.ico)\0*.ico\0图片 (*.png;*.jpg;*.bmp)\0*.png;*.jpg;*.jpeg;*.bmp;*.gif\0所有文件 (*.*)\0*.*\0\0"
    } else {
        "程序 (*.exe;*.lnk)\0*.exe;*.lnk\0所有文件 (*.*)\0*.*\0\0"
    };
    let filter: Vec<u16> = spec.encode_utf16().collect();
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
    // 子进程拿不到主进程的托盘表，用启动时读到的快照
    if let Some(s) = runtime_snap() {
        return s.tray;
    }
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
