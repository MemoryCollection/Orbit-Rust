//! trayhost 独立验证：直接引入产品模块，起假托盘，打印收到的图标表。
//! 用法：trayhostcheck [秒数]

#![allow(dead_code)]

// ---- 产品代码的依赖桩 ----
#[macro_export]
macro_rules! tlog { ($($a:tt)*) => { println!($($a)*) }; }
#[macro_export]
macro_rules! vlog { ($($a:tt)*) => { println!($($a)*) }; }
mod tray {
    pub fn exe_of_pid(_pid: u32) -> String { String::new() }
}
#[path = "../../../src/trayhost.rs"]
mod trayhost;

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(15);
    println!("=== trayhostcheck 启动，监听 {secs}s ===");
    let Some(host) = trayhost::start() else { println!("启动失败"); return };
    let mut last_gen = u64::MAX;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        let gen = host.generation();
        if gen != last_gen {
            last_gen = gen;
            let icons = host.snapshot();
            println!("--- gen={gen} icons={} ---", icons.len());
            for i in &icons {
                println!("  tip={:?} hwnd={:#x} uid={} msg={:#x} v{} flags={:#x} hicon={:#x} guid={:?}",
                    i.tip, i.hwnd, i.uid, i.msg, i.ver, i.flags, i.hicon, i.guid);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    println!("=== 停止，恢复真托盘 ===");
    host.stop();
}
