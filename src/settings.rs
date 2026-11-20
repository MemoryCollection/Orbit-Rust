//! 可调设置：`<exe>/data/settings.txt`，一行一个 `key=value`。
//! 启动时读一次；运行时每 500ms 查一次文件是否被改动，改了就热更新并重绘，
//! 所以后续设置页面只需要写这个文件（或调用这里的 set_*）。
//!
//! 当前键：
//! - `weather_text_scale` 天气面板字号倍率（1.0 = 基准，0.8 ~ 1.8）
//! - `lyric_position`     音乐条位置：`right`（默认，贴胶囊右侧）/ `left` / `bottom`
//! - `lyric_size`         歌词字号（逻辑单位，10 ~ 26，默认 14）

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use crate::config as cfg;
use crate::store;

/// 天气字号倍率上下限（再小看不清，再大卡片会被挤出工作区）
const SCALE_MIN: f32 = 0.8;
const SCALE_MAX: f32 = 1.8;
/// 歌词字号上下限
const LYRIC_MIN: f32 = 10.0;
const LYRIC_MAX: f32 = 26.0;
/// AtomicU32 的 0 表示"还没加载"
const UNLOADED: u32 = 0;

static SCALE: AtomicU32 = AtomicU32::new(UNLOADED);
static LYRIC_SIZE: AtomicU32 = AtomicU32::new(UNLOADED);
/// 歌词位置编码：1 = 左，2 = 右，3 = 下（0 = 未加载）
static LYRIC_POS: AtomicU32 = AtomicU32::new(UNLOADED);
static MTIME: AtomicU64 = AtomicU64::new(0);

/// 音乐条贴在胶囊的哪一侧
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LyricPos {
    Left,
    Right,
    Bottom,
}

impl LyricPos {
    fn code(self) -> u32 {
        match self {
            LyricPos::Left => 1,
            LyricPos::Right => 2,
            LyricPos::Bottom => 3,
        }
    }

    fn from_code(c: u32) -> Self {
        match c {
            1 => LyricPos::Left,
            3 => LyricPos::Bottom,
            _ => LyricPos::Right,
        }
    }

    /// 接受英文键与中文写法，写错了就忽略（保留原值）
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "left" | "l" | "左" | "左侧" => Some(LyricPos::Left),
            "right" | "r" | "右" | "右侧" => Some(LyricPos::Right),
            "bottom" | "b" | "下" | "下方" => Some(LyricPos::Bottom),
            _ => None,
        }
    }
}

fn path() -> PathBuf {
    store::path("settings.txt")
}

/// 启动时调用：文件不存在就写一份默认值，老文件补齐新增键，然后载入
pub fn init() {
    let fresh = ensure_file();
    if !fresh {
        append_missing();
    }
    load();
    MTIME.store(mtime(), Ordering::Relaxed);
}

/// 天气面板字号倍率（乘在 config::WEATHER_* 基准字号上）
pub fn weather_text_scale() -> f32 {
    match SCALE.load(Ordering::Relaxed) {
        UNLOADED => cfg::WEATHER_TEXT_SCALE,
        bits => f32::from_bits(bits),
    }
}

/// 音乐条位置（默认胶囊右侧）
pub fn lyric_position() -> LyricPos {
    match LYRIC_POS.load(Ordering::Relaxed) {
        UNLOADED => LyricPos::Right,
        c => LyricPos::from_code(c),
    }
}

/// 歌词字号（逻辑单位）
pub fn lyric_size() -> f32 {
    match LYRIC_SIZE.load(Ordering::Relaxed) {
        UNLOADED => cfg::LYRIC_FONT_SIZE,
        bits => f32::from_bits(bits),
    }
}

/// 文件被改过就重新载入；返回 true 表示值确实变了（需要重绘）
pub fn reload_if_changed() -> bool {
    let m = mtime();
    if m == MTIME.load(Ordering::Relaxed) {
        return false;
    }
    let before = (weather_text_scale(), lyric_position(), lyric_size());
    load();
    MTIME.store(m, Ordering::Relaxed);
    before != (weather_text_scale(), lyric_position(), lyric_size())
}

// ---------- 文件 ----------

const BODY: &str = "\
# Orbit 设置：一行一个 key=value，保存后立即生效
# 天气面板字号倍率：1.0 为基准
weather_text_scale=1.25
# 音乐条位置：right（胶囊右侧，默认）/ left（左侧）/ bottom（下方）
lyric_position=right
# 歌词字号
lyric_size=14
";

/// 文件不存在就写一份默认值；返回 true 表示是新建的
fn ensure_file() -> bool {
    let p = path();
    if p.exists() {
        return false;
    }
    let _ = std::fs::write(p, BODY);
    true
}

/// 老版本的文件只有 weather_text_scale，把新增键补到末尾，方便直接编辑
fn append_missing() {
    let p = path();
    let Ok(raw) = std::fs::read_to_string(&p) else { return };
    let mut add = String::new();
    for (key, line) in [
        ("lyric_position", "# 音乐条位置：right（胶囊右侧，默认）/ left（左侧）/ bottom（下方）\nlyric_position=right\n"),
        ("lyric_size", "# 歌词字号\nlyric_size=14\n"),
    ] {
        if !raw.contains(key) {
            add.push_str(line);
        }
    }
    if !add.is_empty() {
        let _ = std::fs::write(&p, format!("{raw}{add}"));
    }
}

fn load() {
    let Ok(raw) = std::fs::read_to_string(path()) else {
        SCALE.store(cfg::WEATHER_TEXT_SCALE.to_bits(), Ordering::Relaxed);
        LYRIC_SIZE.store(cfg::LYRIC_FONT_SIZE.to_bits(), Ordering::Relaxed);
        LYRIC_POS.store(LyricPos::Right.code(), Ordering::Relaxed);
        return;
    };
    let mut scale = cfg::WEATHER_TEXT_SCALE;
    let mut size = cfg::LYRIC_FONT_SIZE;
    let mut pos = LyricPos::Right;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "weather_text_scale" => {
                if let Some(n) = parse_num(v) {
                    scale = n.clamp(SCALE_MIN, SCALE_MAX);
                }
            }
            "lyric_size" => {
                if let Some(n) = parse_num(v) {
                    size = n.clamp(LYRIC_MIN, LYRIC_MAX);
                }
            }
            "lyric_position" => {
                if let Some(p) = LyricPos::parse(v) {
                    pos = p;
                }
            }
            _ => {}
        }
    }
    SCALE.store(scale.to_bits(), Ordering::Relaxed);
    LYRIC_SIZE.store(size.to_bits(), Ordering::Relaxed);
    LYRIC_POS.store(pos.code(), Ordering::Relaxed);
}

fn parse_num(v: &str) -> Option<f32> {
    v.trim().parse::<f32>().ok().filter(|n| n.is_finite())
}

fn mtime() -> u64 {
    std::fs::metadata(path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_both_languages() {
        assert_eq!(LyricPos::parse("LEFT"), Some(LyricPos::Left));
        assert_eq!(LyricPos::parse(" 下方 "), Some(LyricPos::Bottom));
        assert_eq!(LyricPos::parse("middle"), None);
    }

    #[test]
    fn unknown_code_falls_back_to_right() {
        assert_eq!(LyricPos::from_code(99), LyricPos::Right);
        assert_eq!(LyricPos::from_code(3), LyricPos::Bottom);
        assert_eq!(LyricPos::from_code(2), LyricPos::Right);
    }
}
