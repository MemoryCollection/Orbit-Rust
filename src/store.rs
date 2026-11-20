//! 唯一的数据落盘入口：所有持久化文件都在 <exe 所在目录>/data/ 下。
//! 不写注册表、不写 %APPDATA%、不写任何安装目录以外的地方。

use std::path::PathBuf;

/// 数据目录（不存在则创建）
pub fn data_dir() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("data");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 数据目录下的文件路径
pub fn path(name: &str) -> PathBuf {
    data_dir().join(name)
}
