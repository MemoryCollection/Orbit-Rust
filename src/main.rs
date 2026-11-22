// 不走控制台子系统：双击 exe 不会再弹出黑色 cmd 窗口（日志见 trace.rs）
#![windows_subsystem = "windows"]

//! 程序入口：初始化日志、解析参数并启动应用。

mod app;
mod appicon;
mod appmsg;
mod clock;
mod config;
mod dock;
mod dockview;
mod egui_soft;
mod guard;
mod traymsg;
mod trayhost;
mod flyout;
mod clipboard;
mod hide;
mod json;
mod lyric;
mod mail;
mod mailview;
mod music;
mod musicbar;
mod musicbarview;
mod net;
mod notify;
mod notifycenter;
mod wintoast;
mod panel;
mod panelview;
mod pillui;
mod render;
mod settings;
mod settingsui;
mod startmenu;
mod store;
mod syspanel;
mod widgetbarview;
mod theme;
mod trace;
mod tray;
mod traydetect;
mod banner;
mod wechatmsg;
mod weather;
mod weathercard;
mod widgetbar;

mod window;

fn main() {
    trace::init();
    let args: Vec<String> = std::env::args().collect();
    // 退出守护子进程：等父进程退出，异常退出时恢复任务栏并重启 explorer（见 guard.rs）
    if let Some(a) = args.iter().find(|a| a.starts_with("--guardian=")) {
        let pid: u32 = a.trim_start_matches("--guardian=").parse().unwrap_or(0);
        if pid != 0 {
            guard::run_guardian(pid);
        }
        return;
    }
    // 进程被强杀后任务栏会停在透明状态，这个开关用来手动恢复
    if args.iter().any(|a| a == "--restore-taskbar") {
        hide::force_restore();
        tlog!("已恢复系统任务栏");
        return;
    }
    // 设置页子进程：winit 不允许同一进程重建事件循环，所以设置页单独起一个进程，
    // 关闭时进程整个退出 → GL 上下文、字体图集、UI 内存全部归还；再打开就是新进程。
    // 必须在单实例互斥之前分支，否则会被当成"重复启动"直接退出。
    if args.iter().any(|a| a == "--settings-ui") {
        settingsui::run_child();
        return;
    }
    // 单实例互斥：已有 orbit 在跑就直接退出（守护子进程 / --restore-taskbar 不受影响）
    if guard::acquire_single_instance().is_err() {
        tlog!("orbit 已在运行，本次启动不重复拉起");
        return;
    }
    // 默认接管任务栏；--show-taskbar 保留原生任务栏（调试/临时用）
    let hide_taskbar = !args.iter().any(|a| a == "--show-taskbar");
    tlog!(
        "启动 orbit（hide_taskbar={hide_taskbar}，开机已运行 {}s，启动来源 {}）",
        guard::uptime_secs(),
        if guard::uptime_secs() < 180 { "疑似开机自启" } else { "手动/复活" }
    );
    app::run(hide_taskbar);
    tlog!("退出");
}
