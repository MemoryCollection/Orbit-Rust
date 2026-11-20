//! 邮箱 widget 的数据层：IMAP over TLS 拉最近 N 封邮件，抽出验证码。
//! 账号信息从 <exe>/data/mail.txt 读取（首次运行由 config.rs 的默认值写入）。
//! 已读是双向的：拉取时读服务端 `\Seen`，用户点开邮件时后台 `UID STORE +FLAGS (\Seen)`
//! 写回服务器（同时在 <exe>/data/mail_read.txt 留一份本地记录，断网也不丢）。
//! 协议实现是"够用就行"的最小 IMAP4rev1 子集：LOGIN / SELECT / FETCH / STORE / LOGOUT。

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use native_tls::TlsConnector;

use crate::config as cfg;
use crate::store;

pub struct MailItem {
    /// IMAP UID，作为持久化已读状态的键（取不到时是内容哈希，带 h 前缀）
    pub uid: String,
    pub from: String,
    pub subject: String,
    pub code: Option<String>,
    pub read: bool,
    /// 服务器上本来就是 `\Seen`（别的客户端读过）
    seen_on_server: bool,
}

/// 后台把已读写回服务器
enum SeenCmd {
    Mark(String),
}

/// 胶囊上邮箱 widget 要显示的东西
pub enum Badge {
    /// 刚识别到的验证码（点击后复制并收回）
    Code(String),
    /// 有未读邮件但没识别到验证码
    Unread,
    /// 只显示图标
    None,
}

pub struct Mail {
    pub items: Vec<MailItem>,
    pub error: Option<String>,
    /// 最近一次拿到验证码的时间（胶囊只在 10 分钟内显示它）
    code_at: Option<Instant>,
    /// 验证码被点击复制过：收回，改回图标
    consumed: bool,
    /// 已读 UID 集合（持久化）
    read: HashSet<String>,
    /// 启动后第一次拉取只建立基线：已有邮件的验证码不算"新识别到"
    baseline_done: bool,
    /// 把 \Seen 写回服务器的工作线程
    seen_tx: Sender<SeenCmd>,
    rx: Option<Receiver<Result<Vec<MailItem>, String>>>,
    next_at: Instant,
    failures: u32,
}

impl Default for Mail {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            error: None,
            code_at: None,
            consumed: false,
            read: load_read(),
            baseline_done: false,
            seen_tx: spawn_seen_worker(),
            rx: None,
            next_at: Instant::now(),
            failures: 0,
        }
    }
}

impl Mail {
    /// 驱动状态机；返回 true 表示界面需要重绘。
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(Ok(mut items)) => {
                    for m in items.iter_mut() {
                        m.read = m.seen_on_server || self.read.contains(&m.uid);
                    }
                    // 新邮件里的验证码才算"新鲜"（首次拉取只建立基线）
                    let fresh = items.first().and_then(|m| m.code.clone());
                    let old = self.items.first().and_then(|m| m.code.clone());
                    if !self.baseline_done {
                        self.baseline_done = true;
                    } else if fresh.is_some() && fresh != old {
                        self.code_at = Some(Instant::now());
                        self.consumed = false;
                    }
                    self.items = items;
                    self.error = None;
                    self.failures = 0;
                    self.rx = None;
                    self.next_at = Instant::now() + Duration::from_secs(cfg::MAIL_INTERVAL);
                    changed = true;
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    self.failures = (self.failures + 1).min(4);
                    self.rx = None;
                    let wait = 120u64 * 2u64.pow(self.failures - 1);
                    self.next_at = Instant::now() + Duration::from_secs(wait.min(900));
                    changed = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.rx = None,
            }
        }

        if self.rx.is_none() && Instant::now() >= self.next_at {
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let out = match creds() {
                    Some(c) => fetch(c),
                    None => Err("未配置邮箱".into()),
                };
                let _ = tx.send(out);
            });
            self.rx = Some(rx);
        }
        changed
    }

    /// 胶囊当前该显示验证码 / "有新邮件" / 只有图标
    pub fn badge(&self) -> Badge {
        if !self.consumed {
            if let Some(at) = self.code_at {
                if at.elapsed() < Duration::from_secs(600) {
                    if let Some(code) = self.items.first().and_then(|m| m.code.as_ref()) {
                        return Badge::Code(code.clone());
                    }
                }
            }
        }
        if self.unread() > 0 {
            Badge::Unread
        } else {
            Badge::None
        }
    }

    /// 验证码被点击复制：收回，回到图标
    pub fn consume_code(&mut self) {
        self.consumed = true;
        self.code_at = None;
    }

    pub fn unread(&self) -> usize {
        self.items.iter().filter(|m| !m.read).count()
    }

    /// 面板某标签页下可见的条目在 items 里的下标（0 = 未读，1 = 全部）
    pub fn visible(&self, tab: usize) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, m)| tab == 1 || !m.read)
            .map(|(i, _)| i)
            .collect()
    }

    /// 点开一封邮件：本地记已读并落盘，同时让后台把 `\Seen` 写回服务器
    pub fn mark_read(&mut self, uid: &str) {
        for m in self.items.iter_mut() {
            if m.uid == uid {
                m.read = true;
            }
        }
        if self.read.insert(uid.to_string()) {
            save_read(&self.read);
        }
        // 内容哈希兜底的键不是真 UID，不能拿去 STORE
        if !uid.is_empty() && uid.chars().all(|c| c.is_ascii_digit()) {
            let _ = self.seen_tx.send(SeenCmd::Mark(uid.to_string()));
        }
    }
}

// ---------- 已读状态 ----------

fn read_path() -> std::path::PathBuf {
    store::path("mail_read.txt")
}

fn load_read() -> HashSet<String> {
    std::fs::read_to_string(read_path())
        .map(|raw| raw.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

fn save_read(set: &HashSet<String>) {
    let body: String = set.iter().map(|u| format!("{u}\n")).collect();
    let _ = std::fs::write(read_path(), body);
}

// ---------- 账号 ----------

/// (host, port, user, pass) —— 首次运行把 config.rs 的默认值写进 data/mail.txt
fn creds() -> Option<(String, u16, String, String)> {
    let path = store::path("mail.txt");
    if !path.exists() {
        let body = format!(
            "# Orbit 邮箱配置（IMAP）。QQ 邮箱需在设置里开启 IMAP 并使用授权码，不是登录密码。\n\
             host={}\nport={}\nuser={}\npass={}\n",
            cfg::MAIL_HOST, cfg::MAIL_PORT, cfg::MAIL_USER, cfg::MAIL_PASS
        );
        let _ = std::fs::write(&path, body);
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let mut host = String::new();
    let mut port = 993u16;
    let mut user = String::new();
    let mut pass = String::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "host" => host = v.trim().to_string(),
            "port" => port = v.trim().parse().unwrap_or(993),
            "user" => user = v.trim().to_string(),
            "pass" => pass = v.trim().to_string(),
            _ => {}
        }
    }
    if host.is_empty() || user.is_empty() || pass.is_empty() {
        return None;
    }
    Some((host, port, user, pass))
}

// ---------- IMAP ----------

struct Response {
    /// 文本部分，字面量被替换为标记
    text: String,
    /// 原文：每个 {N} 对应的 N 字节
    literals: Vec<Vec<u8>>,
}

struct Conn {
    io: BufReader<native_tls::TlsStream<TcpStream>>,
}

impl Conn {
    fn open(host: &str, port: u16) -> Result<Self, String> {
        let stream = TcpStream::connect((host, port)).map_err(|e| format!("连接 {host}:{port} 失败: {e}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(25))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(25))).ok();
        let tls = TlsConnector::new().map_err(|e| format!("TLS 初始化失败: {e}"))?;
        let stream = tls.connect(host, stream).map_err(|e| format!("TLS 握手失败: {e}"))?;
        Ok(Self { io: BufReader::new(stream) })
    }

    fn send(&mut self, line: &str) -> Result<(), String> {
        let w = self.io.get_mut();
        w.write_all(format!("{line}\r\n").as_bytes())
            .and_then(|_| w.flush())
            .map_err(|e| format!("发送失败: {e}"))
    }

    /// 读一行（不含 CRLF）
    fn line(&mut self) -> Result<Vec<u8>, String> {
        let mut v = Vec::new();
        let n = self.io.read_until(b'\n', &mut v).map_err(|e| format!("读取失败: {e}"))?;
        if n == 0 {
            return Err("服务器关闭了连接".into());
        }
        while matches!(v.last(), Some(b'\n') | Some(b'\r')) {
            v.pop();
        }
        Ok(v)
    }

    /// 读一条完整响应（含所有字面量）
    fn response(&mut self) -> Result<Response, String> {
        let mut text = String::new();
        let mut literals: Vec<Vec<u8>> = Vec::new();
        loop {
            let line = self.line()?;
            match literal_len(&line) {
                Some(n) => {
                    text.push_str(&String::from_utf8_lossy(&line));
                    text.push_str(&format!("<{}>", literals.len()));
                    let mut buf = vec![0u8; n];
                    self.io.read_exact(&mut buf).map_err(|e| format!("读取字面量失败: {e}"))?;
                    literals.push(buf);
                }
                None => {
                    text.push_str(&String::from_utf8_lossy(&line));
                    return Ok(Response { text, literals });
                }
            }
        }
    }

    /// 读到 tag 开头的响应为止
    fn until(&mut self, tag: &str) -> Result<(Vec<Response>, String), String> {
        let mut all = Vec::new();
        for _ in 0..2000 {
            let r = self.response()?;
            let ok = r.text.starts_with(tag)
                && r.text.as_bytes().get(tag.len()).map(|b| *b == b' ').unwrap_or(false);
            all.push(r);
            if ok {
                let last = all.last().map(|r| r.text.clone()).unwrap_or_default();
                return Ok((all, last));
            }
        }
        Err("服务端响应异常".into())
    }
}

/// 行尾是 `{123}` 时返回 123
fn literal_len(line: &[u8]) -> Option<usize> {
    if !line.ends_with(b"}") {
        return None;
    }
    let start = line.iter().rposition(|&b| b == b'{')?;
    std::str::from_utf8(&line[start + 1..line.len() - 1]).ok()?.trim().parse().ok()
}

/// 建连接 → 登录 → 选中 INBOX，返回 (连接, 邮件总数)
fn connect(account: &(String, u16, String, String)) -> Result<(Conn, usize), String> {
    let (host, port, user, pass) = account;
    let mut c = Conn::open(host, *port)?;
    let greet = c.line()?;
    if !String::from_utf8_lossy(&greet).starts_with("* OK") {
        return Err(format!("服务器拒绝连接: {}", String::from_utf8_lossy(&greet)));
    }

    c.send(&format!("A1 LOGIN \"{}\" \"{}\"", escape(user), escape(pass)))?;
    let (_, done) = c.until("A1")?;
    if !done.starts_with("A1 OK") {
        return Err(clean_status(&done));
    }

    // SELECT（不是 EXAMINE）：后面要 STORE 已读标记
    c.send("A2 SELECT \"INBOX\"")?;
    let (sel, done) = c.until("A2")?;
    if !done.starts_with("A2 OK") {
        return Err(clean_status(&done));
    }
    let exists = sel
        .iter()
        .filter_map(|r| {
            let t = r.text.trim_start_matches('*').trim();
            let (num, rest) = t.split_once(' ')?;
            rest.trim().to_ascii_uppercase().contains("EXISTS").then(|| num.parse::<usize>().ok())?
        })
        .last()
        .unwrap_or(0);
    Ok((c, exists))
}

fn fetch(account: (String, u16, String, String)) -> Result<Vec<MailItem>, String> {
    let (mut c, exists) = connect(&account)?;
    if exists == 0 {
        let _ = c.send("A9 LOGOUT");
        return Ok(Vec::new());
    }

    let want = cfg::MAIL_FETCH.min(exists);
    let from = exists - want + 1;
    c.send(&format!(
        "A3 FETCH {from}:{exists} (UID FLAGS BODY.PEEK[HEADER.FIELDS (FROM SUBJECT DATE CONTENT-TYPE CONTENT-TRANSFER-ENCODING)] BODY.PEEK[TEXT]<0.4096>)"
    ))?;
    let (all, done) = c.until("A3")?;
    let _ = c.send("A9 LOGOUT");
    if !done.starts_with("A3 OK") {
        return Err(clean_status(&done));
    }

    let mut items = Vec::new();
    for r in all.iter().filter(|r| r.literals.len() >= 2) {
        let header = bytes_to_string(&r.literals[0]);
        let body = &r.literals[1];
        let subject = decode_words(&header_field(&header, "Subject").unwrap_or_default());
        let from = decode_words(&header_field(&header, "From").unwrap_or_default());
        let charset = header
            .to_ascii_lowercase()
            .split("charset=")
            .nth(1)
            .map(|s| s.trim_matches(|c: char| c == '"' || c == ';' || c == ' ').to_string());
        let cte = header_field(&header, "Content-Transfer-Encoding").unwrap_or_default().to_lowercase();
        let text = body_text(body, charset.as_deref(), &cte);
        let code = find_code(&subject).or_else(|| find_code(&text));
        let subject = clean_subject(&subject);
        let from = clean_from(&from);
        // 服务端没给 UID 时用内容哈希兜底（仍能跨重启保持已读状态）
        let uid = uid_of(&r.text).unwrap_or_else(|| format!("h{:016x}", hash64(&from, &subject)));
        let seen = has_seen(&r.text);
        items.push(MailItem { uid, from, subject, code, read: false, seen_on_server: seen });
    }
    items.reverse(); // 最新的在前
    Ok(items)
}

/// 响应里的 `FLAGS (\Seen ...)` 是否含已读
fn has_seen(text: &str) -> bool {
    let up = text.to_ascii_uppercase();
    match up.find("FLAGS") {
        Some(p) => up[p..].split(')').next().unwrap_or_default().contains("\\SEEN"),
        None => false,
    }
}

// ---------- 已读写回服务器 ----------

/// 常驻线程：持有可选连接，收到命令就 `UID STORE +FLAGS (\Seen)`。
/// 失败就丢掉连接，下次点击重新登录（不做重试，避免后台反复敲服务器）。
fn spawn_seen_worker() -> Sender<SeenCmd> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("orbit-mail-seen".into())
        .spawn(move || {
            let Some(account) = creds() else { return };
            let mut conn: Option<Conn> = None;
            let mut tag = 0u32;
            while let Ok(SeenCmd::Mark(uid)) = rx.recv() {
                if conn.is_none() {
                    conn = connect(&account).ok().map(|(c, _)| c);
                }
                let Some(c) = conn.as_mut() else { continue };
                tag += 1;
                let t = format!("S{tag}");
                let ok = c.send(&format!("{t} UID STORE {uid} +FLAGS (\\Seen)")).is_ok()
                    && matches!(c.until(&t), Ok((_, done)) if done.starts_with(&format!("{t} OK")));
                if !ok {
                    conn = None;
                }
            }
        })
        .ok();
    tx
}

/// 从 `* 3 FETCH (UID 1234 BODY[...` 里取 UID
fn uid_of(text: &str) -> Option<String> {
    let up = text.to_ascii_uppercase();
    let p = up.find("UID ")?;
    let d: String = text[p + 4..].chars().take_while(|c| c.is_ascii_digit()).collect();
    (!d.is_empty()).then_some(d)
}

fn hash64(a: &str, b: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for byte in a.as_bytes().iter().chain(b.as_bytes()) {
        h ^= *byte as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn clean_status(s: &str) -> String {
    let t = s.trim();
    match t.split_once(' ') {
        Some((_, rest)) => rest.to_string(),
        None => t.to_string(),
    }
}

// ---------- 头部解析 ----------

fn header_field(header: &str, name: &str) -> Option<String> {
    let mut cur: Option<String> = None;
    for line in header.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(c) = cur.as_mut() {
                c.push(' ');
                c.push_str(line.trim());
            }
            continue;
        }
        if cur.is_some() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case(name) {
                cur = Some(v.trim().to_string());
            }
        }
    }
    cur
}

fn clean_from(s: &str) -> String {
    // "张三" <a@b.com> → 张三；没有显示名则用地址
    if let (Some(l), Some(r)) = (s.find('<'), s.find('>')) {
        if r > l {
            let name = s[..l].trim().trim_matches('"').trim();
            if !name.is_empty() {
                return name.to_string();
            }
            return s[l + 1..r].to_string();
        }
    }
    s.split('@').next().unwrap_or(s).trim_matches('"').to_string()
}

fn clean_subject(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------- 正文 ----------

/// 把 BODY[TEXT] 的字节粗略还原成可读文本（只关心验证码出现在哪）。
fn body_text(raw: &[u8], top_charset: Option<&str>, top_cte: &str) -> String {
    let sniff = String::from_utf8_lossy(&raw[..raw.len().min(600)]).to_ascii_lowercase();
    let charset = sniff
        .split("charset=")
        .nth(1)
        .map(|s| s.trim_matches(|c: char| c == '"' || c == ';' || c == ' ' || c == '?' || c == '>').to_string())
        .or_else(|| top_charset.map(|s| s.to_string()));

    let whole = String::from_utf8_lossy(raw).into_owned();
    let mut out = String::new();

    let mut any_part = false;
    // 按 MIME 边界切开（行首 -- 开头）
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in whole.lines() {
        if line.starts_with("--") && line.len() > 4 {
            if !cur.is_empty() {
                parts.push(cur.clone());
                cur.clear();
            }
            continue;
        }
        cur.push_str(line);
        cur.push('\n');
    }
    if !cur.is_empty() {
        parts.push(cur);
    }

    for part in &parts {
        let Some((head, body)) = part.split_once("\n\n") else { continue };
        if !head.to_ascii_lowercase().contains("content-") {
            continue;
        }
        any_part = true;
        let hl = head.to_ascii_lowercase();
        let cte = if hl.contains("base64") {
            "base64"
        } else if hl.contains("quoted-printable") {
            "qp"
        } else {
            "plain"
        };
        let cs = hl
            .split("charset=")
            .nth(1)
            .map(|s| {
                s.trim_start_matches('"')
                    .split(|c: char| c == '"' || c == ';' || c == '\n' || c == ' ' || c == '\r')
                    .next()
                    .unwrap_or("")
                    .to_string()
            });
        let decoded = match cte {
            "base64" => base64_decode(&body.bytes().filter(|b| !b.is_ascii_whitespace()).collect::<Vec<u8>>()),
            "qp" => qp_decode(body.as_bytes()),
            _ => body.as_bytes().to_vec(),
        };
        out.push_str(&charset_str(&decoded, cs.as_deref().or(charset.as_deref())));
        out.push('\n');
    }

    if !any_part {
        // 单段邮件：用顶层头里的传输编码
        let decoded = match top_cte {
            "base64" => base64_decode(&raw.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect::<Vec<u8>>()),
            "quoted-printable" => qp_decode(raw),
            _ => raw.to_vec(),
        };
        out.push_str(&charset_str(&decoded, charset.as_deref()));
    }
    strip_tags(&out)
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn bytes_to_string(b: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(b) {
        return s.to_string();
    }
    charset_str(b, Some("gbk"))
}

fn charset_str(b: &[u8], charset: Option<&str>) -> String {
    let cs = charset.unwrap_or("utf-8").trim().to_ascii_lowercase();
    let cs = if cs.is_empty() { "utf-8".to_string() } else { cs };
    if let Ok(s) = std::str::from_utf8(b) {
        if cs.starts_with("utf") || cs == "us-ascii" || cs == "ascii" {
            return s.to_string();
        }
    }
    let cp = match cs.as_str() {
        "utf-8" | "utf8" | "us-ascii" | "ascii" => 65001,
        "big5" => 950,
        "iso-8859-1" | "latin1" => 28591,
        _ => 936, // gbk / gb2312 / gb18030
    };
    from_codepage(b, cp)
}

fn from_codepage(b: &[u8], cp: u32) -> String {
    use windows::Win32::Globalization::{MultiByteToWideChar, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
    unsafe {
        let n = MultiByteToWideChar(cp, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), b, None);
        if n <= 0 {
            return String::from_utf8_lossy(b).into_owned();
        }
        let mut buf = vec![0u16; n as usize];
        let r = MultiByteToWideChar(cp, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), b, Some(&mut buf));
        String::from_utf16_lossy(&buf[..r.max(0) as usize])
    }
}

fn qp_decode(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'=' {
            if i + 1 < b.len() && b[i + 1] == b'\n' {
                i += 2;
                continue;
            }
            if i + 2 < b.len() && b[i + 1] == b'\r' && b[i + 2] == b'\n' {
                i += 3;
                continue;
            }
            if i + 2 < b.len() {
                let hi = (b[i + 1] as char).to_digit(16);
                let lo = (b[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn base64_decode(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in b {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// =?UTF-8?B?...?= / =?GBK?Q?...?= 之类的 MIME 编码字解码
fn decode_words(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && i + 1 < bytes.len() && bytes[i + 1] == b'?' {
            if let Some(end) = s[i..].find("?=") {
                let seg = &s[i + 2..i + end];
                let mut it = seg.splitn(3, '?');
                let charset = it.next().unwrap_or("utf-8");
                let enc = it.next().unwrap_or("B").to_ascii_uppercase();
                let data = it.next().unwrap_or("");
                let raw = if enc == "B" {
                    base64_decode(data.as_bytes())
                } else {
                    qp_decode(data.replace('_', " ").as_bytes())
                };
                out.push_str(&charset_str(&raw, Some(charset)));
                i += end + 4;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap_or('?');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

// ---------- 验证码抽取 ----------

/// 关键词附近找 4~8 位数字；找不到再看标题里有没有独立 6 位数字
pub fn find_code(text: &str) -> Option<String> {
    const KEYS: [&str; 8] = ["验证码", "校验码", "动态码", "确认码", "verification code", "verify code", "security code", "code"];
    let lower = text.to_lowercase();
    for key in KEYS {
        let mut from = 0;
        while let Some(p) = lower[from..].find(key) {
            let at = from + p + key.len();
            let tail: String = text[at..].chars().take(48).collect();
            if let Some(code) = digits_in(&tail) {
                return Some(code);
            }
            from = from + p + key.len();
        }
    }
    // 标题里的独立 6 位数字（"123456 是您的验证码"）
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let s = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let n = i - s;
            if n == 6 {
                return Some(text[s..i].to_string());
            }
        } else {
            i += 1;
        }
    }
    None
}

fn digits_in(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let n = i - start;
            if (4..=8).contains(&n) {
                return Some(s[start..i].to_string());
            }
        } else {
            i += 1;
        }
    }
    None
}
