//! 启动流程：GDI+ 初始化、DPI 感知、注册窗口类、创建胶囊窗口、消息循环、退出清理。

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HMENU, LoadCursorW,
    RegisterClassW, SetTimer, ShowWindow, TranslateMessage, IDC_ARROW, MSG, SW_SHOWNOACTIVATE,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::config as cfg;
use crate::dock::Dock;
use crate::hide;
use crate::mailview;
use crate::musicbar;
use crate::panel;
use crate::render;
use crate::settingsui;
use crate::widgetbar;
use crate::window::{wndproc, App};

/// 提示条窗口过程：什么都不做，全部交给默认处理（窗口只是画布）
unsafe extern "system" fn tip_proc(
    hwnd: HWND, msg: u32, w: windows::Win32::Foundation::WPARAM, l: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    DefWindowProcW(hwnd, msg, w, l)
}

/// 托盘托管句柄（整个进程生命周期内持有，退出时 stop 恢复真托盘）
static TRAY_HOST: std::sync::Mutex<Option<crate::trayhost::Host>> = std::sync::Mutex::new(None);

/// 等 explorer 的任务栏窗口出现（开机自启的竞态修复）。
/// Run 键启动早于 shell 就绪时最多等 30s；shell 正常时立即通过。
fn wait_shell_ready() {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    unsafe {
        for i in 0..60u32 {
            if FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).is_ok() {
                if i > 0 {
                    crate::tlog!("[orbit] Shell_TrayWnd 就绪（等待了 {}ms）", i * 500);
                }
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        crate::tlog!("[orbit] 等待 Shell_TrayWnd 超时（30s），explorer 可能未启动，按现状继续");
    }
}

pub fn run(want_hide_taskbar: bool) {
    // 开机自启时，Run 键启动往往早于 explorer 建好任务栏。不等就接管会出问题：
    // hide_all 找不到任务栏（隐藏记录为空、restore 兜不住）、托盘宿主抢注时序错乱，
    // 表现为"开机自启经常失败"。手动启动 / 复活时 shell 已就绪，这里立即通过。
    wait_shell_ready();

    // 对齐 Atrium：检测到第三方托盘宿主（RetroBar / ExplorerPatcher 等）就不争抢，
    // 不隐藏任务栏、不接管托盘，退回 UIA 只读镜像——避免两个宿主抢 Shell_TrayWnd
    // 导致图标丢失 / 程序错识别。
    let third_party = crate::traydetect::third_party_host();
    let hide_taskbar = want_hide_taskbar && third_party.is_none();
    if want_hide_taskbar && third_party.is_some() {
        crate::tlog!("[orbit] 第三方托盘宿主存在，本次以只读模式运行（保留原生任务栏）");
    }

    // panic 退出前恢复原生任务栏（默认 hook 被替换掉了），并留 panic 标记：
    // 守护看到 panic 标记不重启 explorer（任务栏已回来），但会复活 orbit（见 guard.rs）
    std::panic::set_hook(Box::new(|info| {
        if let Some(h) = TRAY_HOST.lock().unwrap().take() { h.stop(); }
        hide::restore_all();
        crate::guard::mark_panic_exit();
        crate::tlog!("[orbit] panic: {info}");
    }));

    let token = render::startup();

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let hinstance = GetModuleHandleW(None).unwrap_or_default();

        // 先接管任务栏再建窗口：Dock 第一次定位就要按"没有任务栏"的屏幕底来算
        if hide_taskbar {
            hide::hide_all();
            hide::install_hook();
        }

        // 托盘托管：假 Shell_TrayWnd + TaskbarCreated，让应用把图标注册到我们这里。
        // 图标来源不依赖 UIA 后，真任务栏是否透明都不影响镜像与悬停。
        if cfg::TRAY_TAKEOVER && cfg::SHOW_TRAY && third_party.is_none() {
            if let Some(host) = crate::trayhost::start() {
                *TRAY_HOST.lock().unwrap() = Some(host);
            } else {
                crate::tlog!("[orbit] 托盘托管启动失败，退回 UIA 镜像");
            }
        }

        // 类必须带鼠标指针：hCursor 为空时系统会给"启动中"的转圈光标
        let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: arrow,
            lpszClassName: w!("ORBIT_CAPSULE"),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let dock_wc = WNDCLASSW {
            lpfnWndProc: Some(crate::dock::wndproc),
            hInstance: hinstance.into(),
            hCursor: arrow,
            lpszClassName: w!("ORBIT_DOCK"),
            ..Default::default()
        };
        RegisterClassW(&dock_wc);

        // 托盘/任务图标的悬停提示条（点击穿透，只要一个默认窗口过程）
        let tip_wc = WNDCLASSW {
            lpfnWndProc: Some(tip_proc),
            hInstance: hinstance.into(),
            lpszClassName: w!("ORBIT_TIP"),
            ..Default::default()
        };
        RegisterClassW(&tip_wc);

        panel::init(hinstance.into());
        mailview::init(hinstance.into());

        let app = Box::new(App::new());
        let app_ptr = Box::into_raw(app);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("ORBIT_CAPSULE"),
            PCWSTR::null(),
            WS_POPUP,
            0, 0, 10, 10,
            HWND::default(),
            HMENU::default(),
            hinstance,
            Some(app_ptr as *const core::ffi::c_void),
        );
        let Ok(hwnd) = hwnd else {
            drop(Box::from_raw(app_ptr));
            render::shutdown(token);
            return;
        };

        (*app_ptr).init(hwnd);
        // 开机启动按设置对齐注册表；接管了任务栏就拉起退出守护子进程
        //（orbit 被强杀时由它重启 explorer 把任务栏拉回来，见 guard.rs）
        crate::guard::sync_autostart();
        if hide_taskbar {
            crate::guard::spawn_guardian();
        }
        // 音乐条要往胶囊窗口回传点击，所以等 hwnd 有了再建
        musicbar::init(hinstance.into(), hwnd);
        // 分离出去的模块条（胶囊左右各一条）同理
        widgetbar::init(hinstance.into(), hwnd);
        // 设置页面（独立小窗，启动时建好、平时隐藏）
        settingsui::init(hinstance.into());
        // 通用右下角横幅（微信消息卡片只是其中一种；读取线程在定时器里跟随设置项启停）
        crate::banner::init(hinstance.into());
        // 接管 Windows 通知中心（开关在设置页；关闭时线程只待命）
        crate::notifycenter::start();
        // 开始菜单（Win 键弹出；快捷方式在设置页配置）
        crate::startmenu::init(hinstance.into());
        // 桌面右上角当地天气卡片（透明分层窗口，数据由主 App 天气 tick 推送）
        crate::weathercard::init(hinstance.into());
        (*app_ptr).render();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        if cfg::SHOW_DOCK {
            let dock_ptr = Box::into_raw(Box::new(Dock::new()));
            if let Ok(dock_hwnd) = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("ORBIT_DOCK"),
                PCWSTR::null(),
                WS_POPUP,
                0, 0, 10, 10,
                HWND::default(),
                HMENU::default(),
                hinstance,
                Some(dock_ptr as *const core::ffi::c_void),
            ) {
                (*dock_ptr).init(dock_hwnd);
                let _ = ShowWindow(dock_hwnd, SW_SHOWNOACTIVATE);
                SetTimer(dock_hwnd, 3, cfg::DOCK_POLL_MS, None);
                // 托盘闪烁（新消息提示）采样：周期短，独立于 2s 的整表重扫
                SetTimer(dock_hwnd, 5, cfg::TRAY_FLASH_MS, None);
                // 设置变化（托盘/返回桌面按钮开关会改 Dock 宽度）500ms 检查一次
                SetTimer(dock_hwnd, 6, 500, None);
            }
        }

        // 时钟 500ms 轮询（分钟变化才重绘）、任务栏隐藏补丁 2s、歌词/音乐条 200ms 对齐
        // （封面旋转用的 40ms 定时器由 App 在播放时才打开；托盘在 Dock 的 2s 定时器里轮询）
        SetTimer(hwnd, 1, 500, None);
        SetTimer(hwnd, 2, 2000, None);
        SetTimer(hwnd, 4, 200, None);
        // 托盘闪烁跟随：Dock 每 70ms 采样真实托盘并发布快照，胶囊按版本号重绘
        SetTimer(hwnd, 6, cfg::TRAY_FLASH_MS, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 正常退出（含 Ctrl+Alt+O 热键）时把系统任务栏还原
        crate::settingsui::shutdown();
        if let Some(h) = TRAY_HOST.lock().unwrap().take() { h.stop(); }
        hide::restore_all();
        // 任务栏已恢复，留收场标记：外部关闭（WM_DESTROY）→ 可复活；
        // Ctrl+Alt+O 正常退出 → clean，守护静默退出（见 guard.rs）
        crate::guard::finish_exit();
        drop(Box::from_raw(app_ptr));
    }

    render::shutdown(token);
}
