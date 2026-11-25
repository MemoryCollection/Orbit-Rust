//! 版本号规则与「检查更新」。
//!
//! 规则（用户约定，勿改）：
//! - 版本显示为四段 `a.b.c.d`；**前三段是一个整体十进制数**（如 129 → 1.2.9、
//!   210 → 2.1.0、200 → 2.0.0、300 → 3.0.0），自动 +1 就是 129→130→131，
//!   也可以直接指定大版本号 200 / 300。
//! - 第四段是开发构建号：每次打包 release 测试版就 +1，**可以无限大**（永不进位）。
//! - 发布正式版时：前三段定好新数（默认 +1，可指定），第四段归零 → `x.y.z.0`，
//!   同时把仓库根目录的 `version.txt` 更新成同样内容推送到 GitHub。
//!
//! 「检查更新」：设置页左下角点版本号 → GET GitHub raw 上的 version.txt，
//! 支持加速前缀（拼在完整 URL 前面）和访问密码（拼 `?pw=...` 在最后），
//! 两者都留空则直连 GitHub。

/// 前三段合起来的数（当前正式版本：211 → 显示 2.1.1）
pub const VER_HEAD: u32 = 211;
/// 第四段构建号（正式版 = 0；打测试包时 +1）
pub const VER_BUILD: u32 = 0;

/// GitHub 上的版本文件（raw 直连地址）
pub const VERSION_URL: &str =
    "https://raw.githubusercontent.com/MemoryCollection/Orbit-Rust/main/version.txt";

/// 前三段拆成 `a.b.c` 显示
fn head_str() -> String {
    format!("{}.{}.{}", VER_HEAD / 100, VER_HEAD / 10 % 10, VER_HEAD % 10)
}

/// 本地版本串，如 "2.1.1.0"
pub fn local() -> String {
    format!("{}.{}", head_str(), VER_BUILD)
}

/// 在文本里找版本号：先按行找整行 `x.y.z.d`；找不到再把整段文本按
/// 「数字和小数点以外」的字符切开逐令牌找（容错 HTML 错误页里夹带的版本串等）。
pub fn parse_remote(text: &str) -> Result<(u32, u32), String> {
    let try_tok = |t: &str| -> Option<(u32, u32)> {
        let parts: Vec<&str> = t.split('.').collect();
        if parts.len() != 4 {
            return None;
        }
        if let (Ok(a), Ok(b), Ok(c), Ok(d)) = (
            parts[0].parse::<u32>(),
            parts[1].parse::<u32>(),
            parts[2].parse::<u32>(),
            parts[3].parse::<u32>(),
        ) {
            Some((a * 100 + b * 10 + c, d))
        } else {
            None
        }
    };
    for line in text.lines() {
        if let Some(v) = try_tok(line.trim()) {
            return Ok(v);
        }
    }
    for tok in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        if let Some(v) = try_tok(tok) {
            return Ok(v);
        }
    }
    Err("版本文件里没有 x.y.z.d 形式的版本号".into())
}

/// 远端是否比本地新：前三段合数更大，或相同且构建号更大
pub fn remote_is_newer(remote: (u32, u32)) -> bool {
    let (rh, rb) = remote;
    rh > VER_HEAD || (rh == VER_HEAD && rb > VER_BUILD)
}

/// 百分号编码：只保留 RFC 3986 的 unreserved 字符，其余按 UTF-8 字节转成 %XX。
/// 密码里带 `&` / `#` / `+` 等字符时，直接拼进 query 会被截断或转义错。
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// 组合校验 URL：`{加速前缀}{GitHub 地址}?pw={密码}`，前缀/密码都可空，
/// 都为空时返回直连 GitHub 的 raw 地址。密码按 URL 规则编码。
pub fn check_url(accel: &str, pw: &str) -> String {
    let mut url = String::new();
    let a = accel.trim();
    if !a.is_empty() {
        url.push_str(a);
        if !url.ends_with('/') {
            url.push('/');
        }
    }
    url.push_str(VERSION_URL);
    let p = pw.trim();
    if !p.is_empty() {
        url.push_str("?pw=");
        url.push_str(&pct_encode(p));
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_version_is_head_dot_build() {
        // 当前定为 211 / build 0 → 2.1.1.0
        assert_eq!(local(), "2.1.1.0");
    }

    #[test]
    fn parse_remote_accepts_plain_line() {
        assert_eq!(parse_remote("2.2.0.7\n"), Ok((220, 7)));
        assert_eq!(parse_remote("1.2.9.12123\r\n"), Ok((129, 12123)));
        assert_eq!(parse_remote("# comment\n3.0.0.0\n"), Ok((300, 0)));
        assert!(parse_remote("hello").is_err());
        // 容错：版本号混在 HTML/错误页文本里也能捞出来
        assert_eq!(parse_remote("<html>404</html> version=3.2.1.9</body>"), Ok((321, 9)));
        // 三段号不算版本
        assert!(parse_remote("raw content v1.2.3 only").is_err());
    }

    #[test]
    fn remote_newer_logic() {
        // 断言相对当前版本，升版时无需改测试
        let h = VER_HEAD;
        assert!(remote_is_newer((h + 1, 0)));
        assert!(remote_is_newer((h, VER_BUILD + 1)));
        assert!(!remote_is_newer((h, VER_BUILD)));
        assert!(!remote_is_newer((h - 1, 99999)));
    }

    #[test]
    fn check_url_composes_accel_and_pw() {
        assert_eq!(check_url("", ""), VERSION_URL);
        // 加速前缀自动补末尾斜杠，密码拼在最后
        assert_eq!(
            check_url("https://proxy.example.com", "secret"),
            "https://proxy.example.com/".to_string() + VERSION_URL + "?pw=secret"
        );
        assert_eq!(
            check_url("https://proxy.example.com/", ""),
            "https://proxy.example.com/".to_string() + VERSION_URL
        );
        assert_eq!(check_url("", "secret"), VERSION_URL.to_string() + "?pw=secret");
        // 特殊字符按 URL 规则编码，避免 query 被截断
        assert_eq!(
            check_url("", "a b&c+d"),
            VERSION_URL.to_string() + "?pw=a%20b%26c%2Bd"
        );
    }
}
