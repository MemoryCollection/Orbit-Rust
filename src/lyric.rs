//! 歌词数据层：按 SMTC 当前曲目（歌名 + 歌手）在网易云音乐搜索并取 LRC，
//! 解析成时间轴后按播放进度取当前行。
//! 只在换歌时请求一次；结果（包括"这首歌确实没有歌词"）在内存里缓存，避免重复请求。
//! 线程模型：每次换歌起一个短命线程去抓，抓完通过 channel 送回主线程。

use std::sync::mpsc::{channel, Receiver, TryRecvError};

use crate::json::Json;
use crate::music::Now;
use crate::net;

/// 缓存的曲目数上限（超出丢最旧的）
const CACHE_MAX: usize = 16;
/// 单次请求超时
const TIMEOUT_MS: i32 = 8000;
/// 曲目键分隔符（歌名与歌手之间）
const SEP: char = '\u{1f}';

/// 一句歌词
#[derive(Clone)]
pub struct Line {
    /// 起始时间（毫秒）
    pub at_ms: u64,
    pub text: String,
}

#[derive(Default)]
pub struct Lyrics {
    /// 当前曲目键（歌名 + 歌手，小写）；None = 没有在播放
    key: Option<String>,
    lines: Vec<Line>,
    /// 正在抓取的曲目（同一时刻最多一个）
    rx: Option<Receiver<(String, Option<Vec<Line>>)>>,
    cache: Vec<(String, Option<Vec<Line>>)>,
}

impl Lyrics {
    /// 驱动换歌 / 收词；返回 true 表示歌词内容变了（需要重绘音乐条）。
    pub fn tick(&mut self, now: Option<&Now>) -> bool {
        let title = now.map(|n| n.title.trim()).unwrap_or("");
        let artist = now.map(|n| n.artist.trim()).unwrap_or("");
        let key = if title.is_empty() {
            None
        } else {
            Some(format!("{}{SEP}{}", title.to_lowercase(), artist.to_lowercase()))
        };

        let mut changed = false;
        if key != self.key {
            self.lines.clear();
            self.rx = None;
            self.key = key.clone();
            changed = true;
            if let Some(k) = key {
                match self.cache.iter().find(|(c, _)| *c == k) {
                    Some((_, hit)) => self.lines = hit.clone().unwrap_or_default(),
                    None => self.request(k, title.to_string(), artist.to_string()),
                }
            }
        }

        if let Some(rx) = self.rx.take() {
            match rx.try_recv() {
                Ok((k, res)) => {
                    if Some(&k) == self.key.as_ref() {
                        self.lines = res.clone().unwrap_or_default();
                        changed = true;
                    }
                    self.cache(k, res);
                }
                Err(TryRecvError::Empty) => self.rx = Some(rx),
                Err(TryRecvError::Disconnected) => {}
            }
        }
        changed
    }

    /// 当前行下标：最后一个起始时间 ≤ pos 的行（早于第一句时返回 0）
    pub fn index_at(&self, pos_ms: u64) -> usize {
        self.lines.partition_point(|l| l.at_ms <= pos_ms).saturating_sub(1)
    }

    /// 当前该显示的那句；前奏（第一句之前）与没有歌词时都返回 None
    pub fn line_text(&self, pos_ms: u64) -> Option<&str> {
        let i = self.index_at(pos_ms);
        let line = self.lines.get(i)?;
        (line.at_ms <= pos_ms).then(|| line.text.as_str())
    }

    fn request(&mut self, key: String, title: String, artist: String) {
        let (tx, rx) = channel();
        let k = key;
        std::thread::Builder::new()
            .name("orbit-lyric".into())
            .spawn(move || {
                let _ = tx.send((k, fetch(&title, &artist)));
            })
            .ok();
        self.rx = Some(rx);
    }

    fn cache(&mut self, key: String, res: Option<Vec<Line>>) {
        if self.cache.iter().any(|(k, _)| *k == key) {
            return;
        }
        self.cache.push((key, res));
        if self.cache.len() > CACHE_MAX {
            self.cache.remove(0);
        }
    }
}

// ---------- 抓取 ----------

/// 搜歌 → 取歌词 → 解析；任何一步失败都当作"没有歌词"
fn fetch(title: &str, artist: &str) -> Option<Vec<Line>> {
    let id = search(title, artist)?;
    let url = format!("https://music.163.com/api/song/lyric?id={id}&lv=-1&kv=-1&tv=-1");
    let raw = net::get(&url, TIMEOUT_MS).ok()?;
    let j = Json::parse(&raw)?;
    let lrc = j.path("lrc.lyric").and_then(|v| v.str()).unwrap_or_default();
    let lines = parse_lrc(lrc);
    (!lines.is_empty()).then_some(lines)
}

/// 搜索并挑最匹配的一首；先用"歌名 歌手"，没结果再只用歌名
fn search(title: &str, artist: &str) -> Option<String> {
    for q in [format!("{title} {artist}"), title.to_string()] {
        let url = format!(
            "https://music.163.com/api/search/get/web?csrf_token=&s={}&type=1&offset=0&total=true&limit=10",
            urlenc(q.trim())
        );
        let Ok(raw) = net::get(&url, TIMEOUT_MS) else { continue };
        let Some(j) = Json::parse(&raw) else { continue };
        let Some(songs) = j.path("result.songs") else { continue };
        if let Some(id) = pick(songs.arr(), title, artist) {
            return Some(id);
        }
    }
    None
}

/// 打分挑歌：歌名完全一致 +4、互相包含 +2（否则不算候选），歌手命中 +3
fn pick(songs: &[Json], title: &str, artist: &str) -> Option<String> {
    let t = norm(title);
    let a = norm(artist);
    let mut best: Option<(i32, i64)> = None;
    for s in songs {
        let name = norm(s.get("name").and_then(|v| v.str()).unwrap_or_default());
        if name.is_empty() || t.is_empty() {
            continue;
        }
        let mut score = if name == t {
            4
        } else if name.contains(&t) || t.contains(&name) {
            2
        } else {
            continue;
        };
        let singers: String = s
            .get("artists")
            .map(|v| v.arr().iter().filter_map(|x| x.get("name").and_then(|n| n.str())).map(norm).collect::<Vec<_>>().join(""))
            .unwrap_or_default();
        if !a.is_empty() && singers.contains(&a) {
            score += 3;
        }
        let Some(id) = s.get("id").and_then(|v| v.int()) else { continue };
        if best.as_ref().map(|(bs, _)| score > *bs).unwrap_or(true) {
            best = Some((score, id));
        }
    }
    best.map(|(_, id)| id.to_string())
}

/// 归一化：只保留字母数字（含中文）并转小写，抹掉空格、括号、标点差异
fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// 百分号编码（只编码非 unreserved 字节）
fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------- LRC 解析 ----------

/// 解析 LRC：每行可带多个 `[mm:ss.xx]` 标签，取标签之后的文字；按时间排序去重
fn parse_lrc(raw: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let mut rest = line.trim();
        let mut stamps: Vec<u64> = Vec::new();
        while let Some(body) = rest.strip_prefix('[') {
            let Some(end) = body.find(']') else { break };
            let Some(ms) = parse_stamp(&body[..end]) else { break };
            stamps.push(ms);
            rest = body[end + 1..].trim_start();
        }
        let text = rest.trim();
        if text.is_empty() || stamps.is_empty() {
            continue;
        }
        for at_ms in stamps {
            out.push(Line { at_ms, text: text.to_string() });
        }
    }
    out.sort_by_key(|l| l.at_ms);
    out.dedup_by_key(|l| l.at_ms);
    out
}

/// `mm:ss` / `mm:ss.x` / `mm:ss.xx` / `mm:ss.xxx` → 毫秒；不是时间标签就返回 None
fn parse_stamp(tag: &str) -> Option<u64> {
    let (m, rest) = tag.split_once(':')?;
    let m: u64 = m.trim().parse().ok()?;
    let (s, frac) = match rest.split_once('.') {
        Some((s, f)) => (s, Some(f)),
        None => (rest, None),
    };
    let s: u64 = s.trim().parse().ok()?;
    let frac = frac.unwrap_or("").chars().take(3).collect::<String>();
    let ms = match frac.len() {
        0 => 0,
        1 => frac.parse::<u64>().ok()? * 100,
        2 => frac.parse::<u64>().ok()? * 10,
        _ => frac.parse::<u64>().ok()?,
    };
    Some(m * 60_000 + s * 1_000 + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc_with_offsets_and_multi_stamp() {
        let raw = "[ti:测试]\n[00:01.50]第一句\n[00:03.2][00:05.75]重复句\n\n[01:02.123]最后一句";
        let lines = parse_lrc(raw);
        let times: Vec<u64> = lines.iter().map(|l| l.at_ms).collect();
        assert_eq!(times, vec![1500, 3200, 5750, 62123]);
        assert_eq!(lines[1].text, "重复句");
    }

    #[test]
    fn picks_line_by_position() {
        let raw = "[00:02.00]A\n[00:05.00]B";
        let l = Lyrics {
            key: None,
            lines: parse_lrc(raw),
            rx: None,
            cache: vec![],
        };
        assert_eq!(l.line_text(1000), None); // 前奏
        assert_eq!(l.line_text(2000), Some("A"));
        assert_eq!(l.line_text(4999), Some("A"));
        assert_eq!(l.line_text(5000), Some("B"));
        assert_eq!(l.index_at(9000), 1);
    }

    #[test]
    fn matches_titles_loosely() {
        assert_eq!(norm("Hello, World! (Live)"), "helloworldlive");
        assert_eq!(urlenc("周杰伦 a"), "%E5%91%A8%E6%9D%B0%E4%BC%A6%20a");
    }
}
