//! 顶部胶囊的时钟模块：日期与时间**合并成一段**文本（一个模块）。
//!
//! 格式是**用户可自由组合的格式串**（strftime 风格），存在 `settings.txt`
//! 的 `clock_format`，例如 `%Y-%m-%d %H:%M:%S`、`%H:%M %m月%d日 %a`。
//! 支持的占位符：
//!   %Y 四位年   %y 两位年   %m 月(02)   %d 日(02)
//!   %H 24小时制(02)          %I 12小时制(02)          %M 分   %S 秒
//!   %p 上午/下午             %a 周五                  %A 星期五
//!   %% 字面 `%`
//! 设置页里直接编辑格式串（Edit 控件），实时预览。
//!
//! 老版本的三件套 `clock_order` / `time_format` / `date_format` 仍会被读取
//! 并自动迁移成一条格式串（见 `settings.rs` 的迁移逻辑）。

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::settings;

// ---------- 格式化 ----------

fn weekday(st: &SYSTEMTIME) -> &'static str {
    ["周日", "周一", "周二", "周三", "周四", "周五", "周六"][(st.wDayOfWeek % 7) as usize]
}

fn weekday_full(st: &SYSTEMTIME) -> &'static str {
    ["星期日", "星期一", "星期二", "星期三", "星期四", "星期五", "星期六"][(st.wDayOfWeek % 7) as usize]
}

/// 12 小时制的小时（0 点 = 12）
fn hour12(st: &SYSTEMTIME) -> u16 {
    match st.wHour % 12 {
        0 => 12,
        h => h,
    }
}

/// 按格式串格式化一个 SYSTEMTIME。不认识的占位符原样保留，方便发现写错。
pub fn format_time(fmt: &str, st: &SYSTEMTIME) -> String {
    let mut out = String::with_capacity(fmt.len() + 16);
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&format!("{:04}", st.wYear)),
            Some('y') => out.push_str(&format!("{:02}", st.wYear % 100)),
            Some('m') => out.push_str(&format!("{:02}", st.wMonth)),
            Some('d') => out.push_str(&format!("{:02}", st.wDay)),
            Some('H') => out.push_str(&format!("{:02}", st.wHour)),
            Some('I') => out.push_str(&format!("{:02}", hour12(st))),
            Some('M') => out.push_str(&format!("{:02}", st.wMinute)),
            Some('S') => out.push_str(&format!("{:02}", st.wSecond)),
            Some('p') => out.push_str(if st.wHour >= 12 { "下午" } else { "上午" }),
            Some('a') => out.push_str(weekday(st)),
            Some('A') => out.push_str(weekday_full(st)),
            Some('%') => out.push('%'),
            // 悬空的 % 或未知占位符：原样吐回去
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// 格式串里是否包含秒（带秒时 500ms 的时钟定时器要按秒重绘）
pub fn shows_seconds() -> bool {
    settings::clock_format().contains("%S")
}

// ---------- 给胶囊 / 设置页用 ----------

fn now() -> SYSTEMTIME {
    unsafe { GetLocalTime() }
}

/// 胶囊里要画的那一段（按当前格式串拼好）
pub fn text() -> String {
    format_time(&settings::clock_format(), &now())
}

/// 设置页预览：某个格式串在"此刻"长什么样
pub fn sample(fmt: &str) -> String {
    format_time(fmt, &now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(y: u16, mo: u16, d: u16, h: u16, mi: u16, s: u16, wd: u16) -> SYSTEMTIME {
        SYSTEMTIME {
            wYear: y,
            wMonth: mo,
            wDay: d,
            wHour: h,
            wMinute: mi,
            wSecond: s,
            wDayOfWeek: wd,
            ..Default::default()
        }
    }

    #[test]
    fn formats() {
        let t = st(2026, 11, 20, 20, 5, 3, 5);
        assert_eq!(format_time("%Y-%m-%d %H:%M:%S", &t), "2026-11-20 20:05:03");
        assert_eq!(format_time("%H:%M", &t), "20:05");
        assert_eq!(format_time("%m月%d日 %a", &t), "11月20日 周五");
        assert_eq!(format_time("%Y年%m月%d日", &t), "2026年11月20日");
        assert_eq!(format_time("%Y/%m/%d", &t), "2026/11/20");
        assert_eq!(format_time("%A", &t), "星期五");
        assert_eq!(format_time("%p %I:%M", &t), "下午 08:05");
        assert_eq!(format_time("%H时%M分", &t), "20时05分");
        // 0 点走 12 小时制要显示 12
        assert_eq!(format_time("%p %I:%M", &st(2026, 1, 2, 0, 9, 0, 5)), "上午 12:09");
        // 字面 %% 与未知占位符原样保留
        assert_eq!(format_time("100%%", &t), "100%");
        assert_eq!(format_time("%Q", &t), "%Q");
        // 悬空的 %
        assert_eq!(format_time("12%", &t), "12%");
        assert_eq!(shows_seconds(), false);
    }
}
