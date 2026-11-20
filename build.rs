//! 将 exe 图标 / 版本信息嵌入到 orbit.exe 中。
//! 图标文件：assets/orbit.ico（多尺寸 256/128/64/48/32/24/16）
//!
//! winresource 默认通过 reg.exe 查询 rc.exe 位置，这里改为直接探测
//! Windows Kits 目录，避免对注册表工具的依赖。

use std::path::PathBuf;

fn find_rc_bin() -> Option<PathBuf> {
    let root = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<_> = std::fs::read_dir(&root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join("x64").join("rc.exe").is_file())
        .collect();
    versions.sort();
    // 取最高版本
    versions.last().map(|v| v.join("x64"))
}

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        if let Some(dir) = find_rc_bin() {
            res.set_toolkit_path(dir.to_str().unwrap());
        }
        res.set_icon("assets/orbit.ico")
            .compile()
            .expect("failed to compile Windows resources (icon)");
    }
}
