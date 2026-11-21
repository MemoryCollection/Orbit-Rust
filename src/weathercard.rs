//! 桌面右上角「当地天气」卡片：独立分层窗口，透明背景，只画文字与图标。
//!
//! 原先天气是胶囊里的一个 widget，点开才在展开面板里看详情。现在按需求把它
//! 移出胶囊，常驻桌面右上角直接显示实况摘要（标题 / 图标+温度 / 天气·城市 /
//! 湿度·风力）。数据由主 App 的 `weather.tick()` 拿到快照后通过 `update()` 推送。
//!
//! - 窗口：`WS_EX_LAYERED|TOOLWINDOW|NOACTIVATE`（**不置顶**），`HTTRANSPARENT`
//!   让鼠标穿透；每次显示都压到 z 序最底（`HWND_BOTTOM`），所以它只贴在桌面上，
//!   任何普通窗口都会盖住它——不会浮在所有窗口上面挡视线。
//! - 透明背景靠分层窗口逐像素 alpha（不画底板），只画字形。
//! - 配色自适应壁纸：沿卡片四周取屏幕像素算平均亮度，深壁纸用浅色字、浅壁纸用
//!   深色字，并给文字加一层反差色的淡描影，混合色壁纸也能读。
//! - 主题 / 字体切换：监听 `WM_SETTINGS_APPLIED` 立即重绘；壁纸明暗每 2s 复检一次。
//! - 显隐：由天气模块的「隐藏」档控制（`module_mode("weather") == Off`），关闭即隐藏。

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClassNameW, GetWindowLongPtrW, IsIconic, IsWindowVisible,
    RegisterClassW, RegisterWindowMessageW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    WindowFromPoint, CREATESTRUCTW, GWLP_USERDATA, HMENU, HTTRANSPARENT, HWND_BOTTOM,
    SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WM_CREATE, WM_NCHITTEST,
    WM_TIMER, WM_WINDOWPOSCHANGED, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
    WNDCLASSW,
};

use eframe::egui;

use crate::config as cfg;
use crate::egui_soft::{argb, bold_font, SoftBackend};
use crate::render;
use crate::settings::{self, ModuleMode};
use crate::weather;

/// 卡片窗口 HWND（raw isize，跨线程安全）
static CARD_HWND: AtomicIsize = AtomicIsize::new(0);
/// 当前要展示的数据；`update()` 写入，窗口过程读取。
static VIEW: Mutex<Option<View>> = Mutex::new(None);

/// `TaskbarCreated`：Explorer 重启后分层样式可能被抹掉，重设一次
static TASKBAR_CREATED: AtomicIsize = AtomicIsize::new(0);

/// 壁纸明暗复检定时器
const TIMER_BG: usize = 1;
/// 复检间隔：壁纸换成浅/深色（或换幻灯片）后最多 2s 跟上
const BG_CHECK_MS: u32 = 2000;
/// 显隐自检定时器：Win+D（显示桌面）会把卡片一起最小化，这里负责恢复
const TIMER_VIS: usize = 2;
/// 自检间隔：按下 Win+D 后最多 500ms 卡片回到桌面层
const VIS_CHECK_MS: u32 = 500;

#[derive(Clone)]
struct View {
    city: String,
    temp: String,
    condition: String,
    icon: String,
    icon_argb: u32,
    /// 详情行（湿度 / 风 / 体感），按顺序拼一行
    details: Vec<String>,
    /// 数据发布时间 / 数据源
    report: String,
}

/// 卡片逻辑尺寸（逻辑点，未乘 DPI 与天气字号倍率）
const CARD_W: f32 = 232.0;
const CARD_H: f32 = 154.0;
/// 距工作区右上角的边距（逻辑点）：不贴边，留出呼吸感
const CARD_MARGIN: f32 = 32.0;

struct Card {
    view: SoftBackend,
    size: (i32, i32),
    pos: (i32, i32),
    /// 壁纸偏深 → 文字用浅色
    dark_bg: bool,
    /// 是否已成功探到壁纸明暗（没探到时用主题默认深色字）
    probed: bool,
}

pub fn init(instance: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: w!("ORBIT_WEATHER_CARD"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let p = Box::into_raw(Box::new(Card {
            view: SoftBackend::new(),
            size: (0, 0),
            pos: (0, 0),
            dark_bg: false,
            probed: false,
        }));
        match CreateWindowExW(
            // 注意：没有 WS_EX_TOPMOST —— 卡片只属于桌面层
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("ORBIT_WEATHER_CARD"),
            PCWSTR::null(),
            WS_POPUP,
            0, 0, 10, 10,
            HWND::default(), HMENU::default(), instance,
            Some(p as *const core::ffi::c_void),
        ) {
            Ok(h) => {
                CARD_HWND.store(h.0 as isize, Ordering::Release);
                // 也登记到统一通知：设置页主题/字体切换后卡片立即重绘
                crate::appmsg::register_weather(h);
                let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
                TASKBAR_CREATED.store(msg as isize, Ordering::Relaxed);
                let _ = SetTimer(h, TIMER_BG, BG_CHECK_MS, None);
                let _ = SetTimer(h, TIMER_VIS, VIS_CHECK_MS, None);
                refresh();
            }
            Err(_) => drop(Box::from_raw(p)),
        }
    }
}

/// 主 App 拿到新天气快照后调用：更新卡片数据并重绘。
/// 任意线程可调用（经 PostMessage 把重绘切回窗口线程）。
pub fn update(snap: &weather::Snapshot) {
    let city = if snap.place.is_empty() { snap.province.clone() } else { snap.place.clone() };
    // 顺序按截图：湿度 · 风力优先，体感放最后（宽度不够时先被截掉）
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("湿度 {}%", snap.humidity));
    if !snap.wind_dir.is_empty() || !snap.wind_power.is_empty() {
        parts.push(format!("{}{}级", snap.wind_dir, snap.wind_power));
    } else if snap.wind > 0 {
        parts.push(format!("风 {} km/h", snap.wind));
    }
    if let Some(f) = snap.feels {
        parts.push(format!("体感 {f}°"));
    }
    let report = if snap.report.is_empty() {
        snap.source.to_string()
    } else {
        format!("{} · {} 发布", snap.source, snap.report)
    };
    let v = View {
        city,
        temp: format!("{}°", snap.temp),
        condition: snap.condition.clone(),
        icon: weather::glyph(snap.code, snap.day).to_string(),
        icon_argb: weather_color(snap.code, snap.day),
        details: parts,
        report,
    };
    if let Ok(mut g) = VIEW.lock() {
        *g = Some(v);
    }
    refresh();
}

/// 设置页点应用 / 主题切换后重绘（颜色取当前主题）；无数据时按开关显隐。
pub fn refresh() {
    let raw = CARD_HWND.load(Ordering::Acquire);
    if raw == 0 {
        return;
    }
    unsafe {
        let h = HWND(raw as *mut core::ffi::c_void);
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            h, crate::appmsg::WM_SETTINGS_APPLIED, WPARAM(0), LPARAM(0),
        );
    }
}

fn weather_color(code: u8, day: bool) -> u32 {
    let t = cfg::cur();
    match code {
        0 => if day { t.sun } else { t.moon },
        1..=3 => t.cloud,
        45 | 48 => t.cloud,
        51..=67 | 80..=82 | 95..=99 => t.rain,
        71..=77 | 85..=86 => t.cloud,
        _ => t.cloud,
    }
}

/// 窗口缩放只按 DPI；天气字号倍率 k 在绘制时单独乘（与 panelview 一致，
/// 否则 k 被平方：窗口放大一截、内容又放大一截，右边和下边就被裁掉）。
fn scale(h: HWND) -> f32 {
    let dpi = unsafe { GetDpiForWindow(h) } as f32 / 96.0;
    dpi.max(0.6)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CREATE {
            let cs = *(l.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return LRESULT(0);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Card;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let card = &mut *ptr;
        // TaskbarCreated：explorer 重启会抹掉分层样式，重建后再摆一次
        let tc = TASKBAR_CREATED.load(Ordering::Relaxed);
        if tc != 0 && msg == tc as u32 {
            card.apply(hwnd);
            return LRESULT(0);
        }
        match msg {
            m if m == crate::appmsg::WM_SETTINGS_APPLIED => {
                // 设置页在独立子进程里写盘，本进程内存是旧值：先重读再按新主题重绘
                crate::settings::reload_if_changed();
                card.apply(hwnd);
                LRESULT(0)
            }
            WM_TIMER if w.0 == TIMER_BG => {
                card.recheck_bg(hwnd);
                LRESULT(0)
            }
            WM_TIMER if w.0 == TIMER_VIS => {
                card.recheck_vis(hwnd);
                LRESULT(0)
            }
            WM_WINDOWPOSCHANGED => LRESULT(0),
            WM_NCHITTEST => {
                // 鼠标完全穿透：不挡桌面和胶囊的点击
                LRESULT(HTTRANSPARENT as isize)
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}

impl Card {
    /// 按当前开关与数据决定显隐，并在显示时重排 + 重绘。
    fn apply(&mut self, h: HWND) {
        let has = VIEW.lock().map(|g| g.is_some()).unwrap_or(false);
        // 复用天气模块的「隐藏」档作为卡片开关；胶囊/左条/右条都表示右上角显示
        let off = settings::module_mode("weather") == ModuleMode::Off;
        if off || !has {
            let _ = unsafe { ShowWindow(h, SW_HIDE) };
            return;
        }
        self.place(h);
        // 先量壁纸明暗（此时窗口可能还没显示，取到的就是纯壁纸），再决定文字色
        self.probe_bg();
        self.render(h);
        let _ = unsafe { ShowWindow(h, SW_SHOWNOACTIVATE) };
        // ShowWindow 可能把窗口抬到前面，再压回桌面层一次
        self.push_bottom(h);
    }

    /// 每 500ms 自检显隐：Win+D（显示桌面）会把本卡片一起最小化，用户要的是
    /// 「天气常驻桌面、不被 Win+D 收走」，所以检测到被收起（最小化或隐藏，且
    /// 开关不是隐藏档）就恢复显示并压回桌面层。返回桌面后卡片最多 500ms 回来。
    fn recheck_vis(&mut self, h: HWND) {
        let off = settings::module_mode("weather") == ModuleMode::Off;
        let has = VIEW.lock().map(|g| g.is_some()).unwrap_or(false);
        if off || !has {
            return; // 开关关着 / 没数据时的隐藏是主动行为，不去拉起来
        }
        let visible = unsafe { IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() };
        if visible {
            return;
        }
        self.place(h);
        let _ = unsafe { ShowWindow(h, SW_SHOWNOACTIVATE) };
        self.push_bottom(h);
        crate::tlog!("[weathercard] 被显示桌面（Win+D）收起，已恢复显示");
    }

    /// 每 2s 复检壁纸明暗：幻灯片/换壁纸后文字色自动跟着切，只有变化时才重绘
    fn recheck_bg(&mut self, h: HWND) {
        if unsafe { !IsWindowVisible(h).as_bool() } || self.size.0 <= 0 {
            return;
        }
        let before = (self.dark_bg, self.probed);
        self.probe_bg();
        if (self.dark_bg, self.probed) != before {
            self.render(h);
        }
    }

    /// 沿卡片四周取屏幕像素估壁纸明暗（正文从 pad 起，边缘一圈只有壁纸）。
    /// 取不到足够有效点时保留上一次结果（被别的窗口挡住时不该乱改颜色）。
    fn probe_bg(&mut self) {
        let (x, y) = self.pos;
        let (w, h) = self.size;
        if w <= 0 || h <= 0 {
            return;
        }
        let mut sum = 0.0f32;
        let mut n = 0u32;
        unsafe {
            let sdc = GetDC(HWND::default());
            if sdc.is_invalid() {
                return;
            }
            let mut sample = |px: i32, py: i32| {
                if !desktop_at(px, py) {
                    return;
                }
                let c = GetPixel(sdc, px, py);
                if c.0 == u32::MAX {
                    return; // CLR_INVALID
                }
                let r = (c.0 & 0xFF) as f32;
                let g = ((c.0 >> 8) & 0xFF) as f32;
                let b = ((c.0 >> 16) & 0xFF) as f32;
                sum += 0.2126 * r + 0.7152 * g + 0.0722 * b;
                n += 1;
            };
            for ins in [3, 7] {
                let l = x + ins;
                let r = x + w - 1 - ins;
                let t = y + ins;
                let b = y + h - 1 - ins;
                let cx = x + w / 2;
                let cy = y + h / 2;
                for (px, py) in [(l, t), (r, t), (cx, t), (l, cy), (r, cy), (l, b), (r, b)] {
                    sample(px, py);
                }
            }
            let _ = ReleaseDC(HWND::default(), sdc);
        }
        if n >= 4 {
            self.dark_bg = sum / n as f32 / 255.0 < 0.58;
            self.probed = true;
        }
    }

    fn place(&mut self, h: HWND) {
        // 窗口物理尺寸 = 逻辑尺寸 × DPI × 天气字号倍率 k（字号在绘制里也乘 k）
        let dpi = scale(h);
        let s = dpi * settings::weather_text_scale();
        let w = (CARD_W * s).round() as i32;
        let hh = (CARD_H * s).round() as i32;
        self.size = (w, hh);
        let mi = crate::window::primary_monitor();
        // 用工作区（有任务栏时不会被遮），没有工作区信息就退回整屏
        let rc = if mi.rcWork.right > mi.rcWork.left && mi.rcWork.bottom > mi.rcWork.top {
            mi.rcWork
        } else {
            mi.rcMonitor
        };
        let m = (CARD_MARGIN * dpi).round() as i32;
        let x = (rc.right - w - m).max(rc.left + m / 2);
        let y = rc.top + m;
        self.pos = (x, y);
        // 不置顶：压到 z 序最底，普通窗口都能盖住它
        unsafe {
            let _ = SetWindowPos(h, HWND_BOTTOM, x, y, w, hh, SWP_NOACTIVATE);
        }
    }

    fn push_bottom(&self, h: HWND) {
        unsafe {
            let _ = SetWindowPos(
                h, HWND_BOTTOM, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    fn render(&mut self, h: HWND) {
        let (w, hh) = self.size;
        let view = match VIEW.lock().ok().and_then(|g| g.clone()) {
            Some(v) => v,
            None => return,
        };
        if w <= 0 || hh <= 0 {
            return;
        }
        let s = scale(h);
        let rgba = paint(&mut self.view, w, hh, s, &view, self.dark_bg);
        let bgra = render::premultiplied_rgba_to_bgra(&rgba);
        render::present(h, &bgra, w, hh);
    }
}

/// 这个点下面是不是桌面（没人盖住）？自己的卡片是穿透的，也会落到桌面
unsafe fn desktop_at(x: i32, y: i32) -> bool {
    let h = WindowFromPoint(POINT { x, y });
    if h.is_invalid() {
        return true;
    }
    let mut buf = [0u16; 64];
    let n = GetClassNameW(h, &mut buf);
    if n <= 0 {
        return true;
    }
    let cls = String::from_utf16_lossy(&buf[..n as usize]);
    matches!(cls.as_str(), "Progman" | "WorkerW" | "ORBIT_WEATHER_CARD" | "")
}

/// 文字绘制（带淡描影）：同一句话用反差色先画一遍，混合壁纸上也能读。
///
/// 描影偏移固定 **1 物理像素**（`1.0 / ppp` 逻辑点）：按字号倍率 k 偏移会得到
/// 1~2 物理像素不等的飘移，小字看起来就像叠了一层虚影（发糊）。固定 1px 后
/// 主字与描影都落在整数像素上，边缘干净。
fn txt(
    p: &egui::Painter,
    x: f32,
    y: f32,
    s: &str,
    font: egui::FontId,
    color: u32,
    shadow: u32,
    ppp: f32,
) {
    let ppp = ppp.max(0.5);
    if shadow != 0 {
        p.text(
            egui::pos2(x, y + 1.0 / ppp),
            egui::Align2::LEFT_TOP,
            s,
            font.clone(),
            argb(shadow),
        );
    }
    p.text(egui::pos2(x, y), egui::Align2::LEFT_TOP, s, font, argb(color));
}

fn ellipsize(soft: &SoftBackend, text: &str, max_w: f32, size: f32) -> String {
    let font = egui::FontId::proportional(size);
    if soft.text_width(text, font.clone()) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut trial = out.clone();
        trial.push(ch);
        trial.push('…');
        if soft.text_width(&trial, font.clone()) > max_w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// 按壁纸明暗给一套字色：主文字 / 次要 / 更淡 / 描影
fn palette(dark_bg: bool) -> (u32, u32, u32, u32) {
    if dark_bg {
        // 深壁纸：近白字 + 深色淡描影
        (0xFF_F4_F7_FC, 0xFF_D2_D9_E6, 0xB3_D2_D9_E6, 0x66_00_00_00)
    } else {
        // 浅壁纸：深字 + 白色淡描影
        (0xFF_16_19_1F, 0xFF_49_50_5A, 0xA6_49_50_5A, 0x66_FF_FF_FF)
    }
}

/// 往白色方向提亮，用于深壁纸上的强调色 / 天气图标
fn lift(c: u32, k: f32) -> u32 {
    let ch = |s: u32| {
        let v = ((c >> s) & 0xFF) as f32;
        ((v * (1.0 - k) + 255.0 * k).round() as u32).min(255)
    };
    (0xFF << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// 透明底板：只画内容，不铺 panel_bg（分层窗口逐像素 alpha 决定透明）。
fn paint(soft: &mut SoftBackend, w: i32, h: i32, scale: f32, v: &View, dark_bg: bool) -> Vec<u8> {
    soft.set_scale(scale);
    let fw = w as f32 / scale;
    let fh = h as f32 / scale;
    let ctx = soft.ctx().clone();
    let input = soft.input(fw, fh);
    let mut out = ctx.run_ui(input, |ui| {
        let p = ui.painter().clone();
        let t = cfg::cur();
        let k = settings::weather_text_scale();
        let (main, dim, faint, shadow) = palette(dark_bg);
        // 强调色跟着壁纸走：深壁纸提亮，浅壁纸保持主题色
        let accent = if dark_bg { lift(t.accent, 0.35) } else { t.accent };
        let icon_color = if dark_bg { lift(v.icon_argb, 0.18) } else { v.icon_argb };

        let pad = 16.0 * k;
        let mut y = 8.0 * k;

        // ① 标题：当地(次) + 天气(强调)
        let title_size = 13.0 * k;
        let title_font = egui::FontId::proportional(title_size);
        let title_h = soft.text_height("当地天气", title_font.clone());
        let w1 = soft.text_width("当地", title_font.clone());
        txt(&p, pad, y, "当地", title_font.clone(), dim, shadow, scale);
        txt(&p, pad + w1, y, "天气", title_font, accent, shadow, scale);
        y += title_h + 10.0 * k;

        // ② 主行：图标 + 大温度（真粗体）+ 右侧「天气 / 城市」两行
        let temp_size = 36.0 * k;
        let icon_size = cfg::WEATHER_ICON_SIZE * k * 0.82;
        let temp_font = bold_font(temp_size);
        let temp_h = soft.text_height(&v.temp, temp_font.clone());
        let icon_h = soft.text_height(&v.icon, egui::FontId::proportional(icon_size));
        let row_h = temp_h.max(icon_h);

        txt(
            &p, pad, y + (row_h - icon_h) * 0.5, &v.icon,
            egui::FontId::proportional(icon_size), icon_color, shadow, scale,
        );
        let temp_x = pad + icon_size + 8.0 * k;
        txt(&p, temp_x, y + (row_h - temp_h) * 0.5, &v.temp, temp_font.clone(), main, shadow, scale);
        let temp_w = soft.text_width(&v.temp, temp_font);

        let right_x = temp_x + temp_w + 12.0 * k;
        let avail = (fw - pad - right_x).max(0.0);
        let cond_size = cfg::WEATHER_COND_SIZE * k;
        let city_size = cfg::WEATHER_CITY_SIZE * k;
        let cond_font = egui::FontId::proportional(cond_size);
        let city_font = egui::FontId::proportional(city_size);
        let cond_h = soft.text_height("晴", cond_font.clone());
        let city_h = soft.text_height("北京市", city_font.clone());
        let stack_h = cond_h + 3.0 * k + city_h;
        let stack_y = y + (row_h - stack_h) * 0.5;
        let cond = ellipsize(soft, &v.condition, avail, cond_size);
        txt(&p, right_x, stack_y, &cond, cond_font, main, shadow, scale);
        let city = ellipsize(soft, &v.city, avail, city_size);
        txt(&p, right_x, stack_y + cond_h + 3.0 * k, &city, city_font, dim, shadow, scale);
        y += row_h + 12.0 * k;

        // ③ 详情行：湿度 / 风 / 体感，一行放不下就省略
        if !v.details.is_empty() {
            let detail_size = cfg::WEATHER_DETAIL_SIZE * k;
            let detail = v.details.join("   ·   ");
            let detail = ellipsize(soft, &detail, fw - pad * 2.0, detail_size);
            let dfont = egui::FontId::proportional(detail_size);
            let dh = soft.text_height("湿", dfont.clone());
            txt(&p, pad, y, &detail, dfont, dim, shadow, scale);
            y += dh + 7.0 * k;
        }

        // ④ 来源行（数据源 · 发布时间）
        if !v.report.is_empty() {
            let rep_size = 9.5 * k;
            let rep = ellipsize(soft, &v.report, fw - pad * 2.0, rep_size);
            txt(&p, pad, y, &rep, egui::FontId::proportional(rep_size), faint, shadow, scale);
        }
        let _ = fh;
    });
    soft.update_textures(&mut out.textures_delta);
    soft.rasterize(out.shapes, out.pixels_per_point, w, h)
}
