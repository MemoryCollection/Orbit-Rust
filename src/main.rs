// 不走控制台子系统：双击 exe 不会再弹出黑色 cmd 窗口（日志见 trace.rs）
#![windows_subsystem = "windows"]

//! 程序入口：初始化日志、解析参数并启动应用。

mod app;
mod appicon;
mod config;
mod dock;
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
mod net;
mod panel;
mod render;
mod settings;
mod store;
mod trace;
mod tray;
mod weather;
mod window;

fn main() {
    trace::init();
    let args: Vec<String> = std::env::args().collect();
    // 进程被强杀后任务栏会停在透明状态，这个开关用来手动恢复
    if args.iter().any(|a| a == "--restore-taskbar") {
        hide::force_restore();
        tlog!("已恢复系统任务栏");
        return;
    }
    // 默认接管任务栏；--show-taskbar 保留原生任务栏（调试/临时用）
    let hide_taskbar = !args.iter().any(|a| a == "--show-taskbar");
    tlog!("启动 orbit（hide_taskbar={hide_taskbar}）");
    app::run(hide_taskbar);
    tlog!("退出");
}
