//! 悬停弹窗重定位：应用在"指针悬停托盘图标"时会自己弹出一个小窗（微信是
//! `Qt51514QWindowToolSaveBits`），位置在**旧托盘角**。Dock 把悬停转发过去后，
//! 这个小窗就会弹在右上角/左上角而不是 Dock 上方，所以这里在悬停开始时盯住那个进程，
//! 找到它的可见 tool window 再用 `SetWindowPos` 摆到 Dock 正上方。
//!
//! 移植自 C# 版 Orbit 的 `Interop/HoverFlyoutRelocator.cs`，并针对"微信双开"做了加固
//! （实测：两个微信实例的悬停卡片都由同一个进程创建，且闪烁时窗口会反复销毁重建）：
//! - 只匹配"最新一次悬停"，但 generation 按**图标 hwnd** 分组，两个实例互不取消；
//! - 弹窗进程可能不是图标宿主进程（双开时图标 A 的卡片归进程 B），所以按 exe 名
//!   匹配兄弟进程；
//! - 弹窗出现有延迟（微信实测 2~3s，且闪烁时反复重建），搜索窗口延长到 4s；
//! - 守位期间窗口若消失（微信换图标 = 销毁重建），在宽限期内重新查找并继续守位；
//! - 摆好后继续盯 20 秒，漂了就再摆；一秒钟内自我复位就反复纠正。
//! - 不碰系统图标（explorer 那批）和抢到前台的窗口（那是用户真正打开的 UI）。
//! - 全程不动光标。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible, SetWindowPos, GWL_EXSTYLE, GWL_STYLE,
    SWP_NOACTIVATE, SWP_NOSENDCHANGING, SWP_NOSIZE, SWP_NOZORDER, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    WS_MAXIMIZE,
};

/// 悬停开始后盯多久找弹窗。微信实测弹窗延迟 2~3s 出现，且闪烁时会重建，给足 4s。
const FIND_MS: u64 = 4000;
const POLL_MS: u64 = 35;
/// 弹窗下沿与 Dock 上沿之间留的缝：1px，看上去像 Dock 的一部分
const GAP: i32 = 1;
/// 离目标位置超过这么多像素才重新摆（保持那条 1px 缝不被破坏）
const DRIFT: i32 = 2;
/// 弹窗开着就一直守着（超时线程自己退出）
const KEEPALIVE_MS: u64 = 20_000;
/// 窗口销毁后重新查找的宽限期：微信闪烁会销毁重建弹窗，新 hwnd 要接着守
const REBUILD_GRACE_MS: u64 = 3500;
/// 有的程序每次都把自己挪回去，别无限纠
const MAX_NUDGES: u32 = 100;

const MIN_W: i32 = 60;
const MIN_H: i32 = 40;
const MAX_W: i32 = 800;
const MAX_H: i32 = 900;

/// 每个图标（hwnd）各自的 generation。双开时两个微信图标各走一条线程，互不取消。
static GENERATIONS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// 是否有任意一条守位线程在活动（用于 stop 时收尾，保留给将来）
static _ACTIVE: AtomicBool = AtomicBool::new(false);

/// 这条悬停线程是否仍是该图标的最新一次。
fn is_current(icon_hwnd: usize, generation: usize) -> bool {
    GENERATIONS
        .lock()
        .unwrap()
        .get(&icon_hwnd)
        .is_some_and(|&g| g == generation)
}

/// 用户开始悬停某个托盘图标了：按需要启动一个后台线程去盯它的悬停弹窗。
///
/// `pid` 是图标宿主窗口所属进程，`icon_hwnd` 是图标自己的窗口句柄（用来分组 generation），
/// `capsule` 是 Dock 的屏幕矩形（弹窗要摆到它上方）。
pub fn watch(pid: u32, icon_hwnd: HWND, capsule: RECT, label: &str) {
    if pid == 0 {
        return;
    }
    if pid == unsafe { windows::Win32::System::Threading::GetCurrentProcessId() } {
        return;
    }
    if capsule.right - capsule.left < 40 || capsule.bottom - capsule.top < 20 {
        crate::vlog!("[flyout] 跳过：Dock 矩形无效");
        return;
    }
    let Some(stem) = exe_stem_of(pid) else {
        return;
    };
    // 音量/网络/电池这类系统外壳图标的弹窗是 Windows 自己的，不能动
    if matches!(
        stem.as_str(),
        "explorer" | "shellexperiencehost" | "startmenuexperiencehost"
    ) {
        return;
    }

    // 按图标 hwnd 递增：同一图标的新悬停取消它自己的旧线程；不同图标互不干扰。
    // 有些图标宿主 hwnd 可能为 0（理论上不会），退化到用 pid 分组。
    let key = if icon_hwnd.0.is_null() {
        usize::MAX - pid as usize
    } else {
        icon_hwnd.0 as usize
    };
    let generation = {
        let mut g = GENERATIONS.lock().unwrap();
        let e = g.entry(key).or_insert(0);
        *e = e.wrapping_add(1);
        *e
    };
    let label = label.to_string();
    crate::tlog!("[flyout] watch pid={pid} icon={:#x} stem={stem} gen={generation}", key);
    _ACTIVE.store(true, Ordering::Relaxed);
    std::thread::spawn(move || run(pid, key, stem, capsule, generation, label));
}

fn run(pid: u32, key: usize, stem: String, capsule: RECT, generation: usize, label: String) {
    let mut names: HashMap<u32, String> = HashMap::new();

    // ---- 找弹窗 ----
    let mut found = HWND::default();
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(FIND_MS) {
        if !is_current(key, generation) {
            return;
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
        if let Some(h) = try_find(pid, &stem, capsule, &mut names) {
            found = h;
            break;
        }
    }
    if found.0.is_null() {
        return; // 这次悬停没弹窗，没得摆
    }

    // ---- 摆好，并守着别让它漂回去 ----
    // 一次 SetWindowPos 不够：有的程序会在 WM_WINDOWPOSCHANGED 或定时器里重新锚定，
    // 表现就是"挪过去一两秒后又回到旧托盘角"。
    let deadline = Instant::now() + Duration::from_millis(KEEPALIVE_MS);
    let mut nudges = 0u32;
    let mut reported = false;
    // 窗口被销毁（微信闪烁换图标）后，给一段宽限期重新找新 hwnd 接着守。
    let mut rebuild_until = Instant::now() + Duration::from_millis(REBUILD_GRACE_MS);
    while Instant::now() < deadline {
        if !is_current(key, generation) {
            return;
        }
        unsafe {
            if !IsWindow(found).as_bool() || !IsWindowVisible(found).as_bool() {
                // 弹窗关了 or 被销毁重建：在宽限期内找新的，找不到就当作悬停结束
                if Instant::now() > rebuild_until {
                    return;
                }
                if let Some(h) = try_find(pid, &stem, capsule, &mut names) {
                    found = h;
                    rebuild_until = Instant::now() + Duration::from_millis(REBUILD_GRACE_MS);
                    nudges = 0;
                    crate::vlog!("[flyout] {label} 弹窗重建 → 新 hwnd={:#x}", found.0 as usize);
                }
                std::thread::sleep(Duration::from_millis(POLL_MS));
                continue;
            }
            // 这里刻意不检查"抢到前台就退出"：微信双开的悬停预览卡片自身就会抢前台，
            // 一退出就没人纠正位置（用户看到的"一个正常、一个弹到左上角"）。
            // 真正用户点击预览卡片会打开微信主窗口，这个 tool 窗随之销毁，上面的
            // IsWindow/IsWindowVisible 检查会让守位线程自然退出。
            let mut cur = RECT::default();
            if GetWindowRect(found, &mut cur).is_err() {
                return;
            }
            let w = cur.right - cur.left;
            let h = cur.bottom - cur.top;
            if w <= 0 || h <= 0 {
                return;
            }
            let (x, y) = target_point(w, h, capsule);
            if (cur.left - x).abs() <= DRIFT && (cur.top - y).abs() <= DRIFT {
                if !reported && nudges > 0 {
                    crate::vlog!("[flyout] {label} 的悬停弹窗已移到 Dock 上方 ({x},{y}) {w}x{h}");
                    reported = true;
                }
                std::thread::sleep(Duration::from_millis(140));
                continue;
            }
            if nudges >= MAX_NUDGES {
                crate::tlog!("[flyout] {label} 的悬停弹窗反复自我复位，放弃纠正");
                return;
            }
            nudges += 1;
            // 不激活、不改 Z 序、不发 WM_WINDOWPOSCHANGING（免得对方又把自己拽回去）
            let _ = SetWindowPos(
                found,
                HWND::default(),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSENDCHANGING,
            );
            if !reported {
                crate::tlog!("[flyout] {label} 的悬停弹窗 → ({x},{y}) {w}x{h}");
                reported = true;
            }
            std::thread::sleep(Duration::from_millis(120));
        }
    }
}

/// 该进程（或同名兄弟进程）里最像悬停弹窗的那个窗口。
///
/// 判据：可见、尺寸合理、不是最大化、没有 `WS_EX_APPWINDOW`、是 tool window
/// （`WS_EX_TOOLWINDOW` 或类名带 "tool"）、没抢前台、且还没摆到位。
/// 满足条件的取**面积最小**的——程序常在真弹窗旁边再开几个辅助窗口。
fn try_find(
    pid: u32,
    stem: &str,
    capsule: RECT,
    names: &mut HashMap<u32, String>,
) -> Option<HWND> {
    struct Ctx<'a> {
        pid: u32,
        stem: &'a str,
        capsule: RECT,
        best: HWND,
        best_area: i64,
        names: &'a mut HashMap<u32, String>,
    }

    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let ctx = &mut *(l.0 as *mut Ctx);
        unsafe {
            if !IsWindowVisible(h).as_bool() {
                return BOOL(1);
            }
            let mut wpid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut wpid));
            if wpid == 0 || wpid == std::process::id() {
                return BOOL(1);
            }
            if !belongs(ctx, wpid) {
                return BOOL(1);
            }
            let mut r = RECT::default();
            if GetWindowRect(h, &mut r).is_err() {
                return BOOL(1);
            }
            let w = r.right - r.left;
            let hh = r.bottom - r.top;
            if w < MIN_W || hh < MIN_H || w > MAX_W || hh > MAX_H {
                return BOOL(1);
            }
            let style = GetWindowLongPtrW(h, GWL_STYLE) as u32;
            if style & WS_MAXIMIZE.0 != 0 {
                return BOOL(1);
            }
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
            if ex & WS_EX_APPWINDOW.0 != 0 {
                return BOOL(1);
            }
            let mut cls = [0u16; 256];
            let n = GetClassNameW(h, &mut cls);
            let cls = String::from_utf16_lossy(&cls[..n as usize]).to_lowercase();
            let is_tool = ex & WS_EX_TOOLWINDOW.0 != 0 || cls.contains("tool");
            if !is_tool {
                return BOOL(1);
            }
            // 注意：不要用"抢到前台"来排除。实测微信双开的悬停预览卡片
            // （Qt51514QWindowToolSaveBits）**会抢前台**，按 C# 原版的这条过滤会把它
            // 漏掉，于是"一个正常、一个弹到左上角"。真正用户打开的界面是主窗口，
            // 它带 WS_EX_APPWINDOW（上面已排除）或不是这种小 tool 窗，不受影响。
            let (x, y) = target_point(w, hh, ctx.capsule);
            if (r.left - x).abs() <= DRIFT && (r.top - y).abs() <= DRIFT {
                return BOOL(1); // 已经摆好了
            }
            let area = w as i64 * hh as i64;
            if area < ctx.best_area {
                ctx.best_area = area;
                ctx.best = h;
            }
        }
        BOOL(1)
    }

    fn belongs(ctx: &mut Ctx, wpid: u32) -> bool {
        if wpid == ctx.pid {
            return true;
        }
        if ctx.stem.is_empty() {
            return false;
        }
        let stem = ctx.stem;
        let name = ctx
            .names
            .entry(wpid)
            .or_insert_with(|| exe_stem_of(wpid).unwrap_or_default());
        // 兄弟进程：exe 名以被悬停进程的名字开头（对齐 Orbit 的 OrdinalIgnoreCase StartsWith）。
        // 微信双开实测：图标宿主 pid 与真正创建卡片的进程不同，但 exe 名都是 weixin，靠这条匹配。
        name.starts_with(stem)
    }

    let mut ctx = Ctx {
        pid,
        stem,
        capsule,
        best: HWND::default(),
        best_area: i64::MAX,
        names,
    };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    if ctx.best.0.is_null() {
        None
    } else {
        Some(ctx.best)
    }
}

/// 弹窗该摆在哪：水平居中于 Dock，下沿距 Dock 上沿 `GAP` 像素，并夹在工作区内。
fn target_point(w: i32, h: i32, capsule: RECT) -> (i32, i32) {
    let center_x = (capsule.left + capsule.right) / 2;
    let mut x = center_x - w / 2;
    let mut y = capsule.top - h - GAP;
    unsafe {
        let mon = MonitorFromPoint(
            windows::Win32::Foundation::POINT {
                x: center_x,
                y: capsule.top,
            },
            MONITOR_DEFAULTTONEAREST,
        );
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !mon.is_invalid() && GetMonitorInfoW(mon, &mut info).as_bool() {
            let work = info.rcWork;
            if x < work.left + GAP {
                x = work.left + GAP;
            }
            if x + w > work.right - GAP {
                x = work.right - GAP - w;
            }
            if y < work.top + GAP {
                y = work.top + GAP;
            }
        }
    }
    (x, y)
}

/// 进程 exe 的文件名（不含扩展名、小写）
fn exe_stem_of(pid: u32) -> Option<String> {
    unsafe {
        let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(
            proc,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(proc);
        r.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let file = path.rsplit(['\\', '/']).next().unwrap_or(&path);
        let stem = file.split('.').next().unwrap_or(file);
        Some(stem.to_lowercase())
    }
}
