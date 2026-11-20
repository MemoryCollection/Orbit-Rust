//! 天气 widget 的数据层。
//! 主数据源：高德（/v3/ip 定城市 + /v3/weather/weatherInfo base/all 拿实况与四天预报）；
//! 高德不可用时回退 Open-Meteo（免 key）。拉取全部在后台线程，失败指数退避。

// 部分字段由面板直接展示，胶囊只用其中一小部分
#![allow(dead_code)]

use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config as cfg;
use crate::json::Json;
use crate::net;
use crate::settings;

pub struct Day {
    pub label: String,
    /// 原始日期 YYYY-MM-DD
    pub date: String,
    /// 归一化到 Open-Meteo 的 weather_code，用于选图标
    pub code: u8,
    pub high: i32,
    pub low: i32,
    /// 降水概率（%），高德不提供 → 0
    pub rain: i32,
    pub day_weather: String,
    pub night_weather: String,
    pub day_wind: String,
    pub night_wind: String,
    pub day_power: String,
    pub night_power: String,
}

pub struct Snapshot {
    pub place: String,
    pub province: String,
    /// 天气现象原文（高德直接给中文，Open-Meteo 由 code 翻译）
    pub condition: String,
    pub temp: i32,
    pub code: u8,
    pub day: bool,
    pub high: i32,
    pub low: i32,
    /// 体感温度（仅 Open-Meteo 提供，无则等于 temp）
    pub feels: Option<i32>,
    pub humidity: i32,
    /// 风速 km/h（仅 Open-Meteo）
    pub wind: i32,
    pub wind_dir: String,
    pub wind_power: String,
    /// 数据发布时间 HH:MM
    pub report: String,
    pub source: &'static str,
    pub days: Vec<Day>,
    /// 高德城市编码（缓存，省一次 /v3/ip 调用）
    pub adcode: String,
    pub lat: f64,
    pub lon: f64,
}

pub struct Weather {
    pub snap: Option<Snapshot>,
    pub error: Option<String>,
    rx: Option<Receiver<Result<Snapshot, String>>>,
    next_at: Instant,
    failures: u32,
    /// 定位缓存（3 小时内复用）
    pos: Option<(f64, f64, String)>,
    pos_at: Option<Instant>,
    /// 高德 adcode 缓存（24 小时内复用）
    adcode: Option<String>,
    adcode_at: Option<Instant>,
}

impl Default for Weather {
    fn default() -> Self {
        Self {
            snap: None,
            error: None,
            rx: None,
            next_at: Instant::now(),
            failures: 0,
            pos: None,
            pos_at: None,
            adcode: None,
            adcode_at: None,
        }
    }
}

impl Weather {
    /// 驱动状态机；返回 true 表示界面需要重绘。
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(Ok(s)) => {
                    if !s.adcode.is_empty() {
                        self.adcode = Some(s.adcode.clone());
                        self.adcode_at = Some(Instant::now());
                    }
                    if s.lat != 0.0 && s.lon != 0.0 {
                        self.pos = Some((s.lat, s.lon, s.place.clone()));
                        self.pos_at = Some(Instant::now());
                    }
                    self.snap = Some(s);
                    self.error = None;
                    self.failures = 0;
                    self.rx = None;
                    self.next_at = Instant::now() + Duration::from_secs(cfg::WEATHER_INTERVAL);
                    changed = true;
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    self.failures = (self.failures + 1).min(5);
                    self.rx = None;
                    let wait = 120u64 * 2u64.pow(self.failures - 1);
                    self.next_at = Instant::now() + Duration::from_secs(wait.min(1800));
                    changed = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.rx = None,
            }
        }

        if self.rx.is_none() && Instant::now() >= self.next_at {
            let pos = self.fresh_pos();
            let adcode = self.fresh_adcode();
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let _ = tx.send(fetch(adcode, pos));
            });
            self.rx = Some(rx);
        }
        changed
    }

    /// 手动刷新（面板上的"刷新"按钮）
    pub fn refresh(&mut self) {
        if self.rx.is_none() {
            self.next_at = Instant::now();
        }
    }

    fn fresh_pos(&self) -> Option<(f64, f64, String)> {
        if !cfg::WEATHER_CITY.is_empty() {
            return None;
        }
        match (self.pos.as_ref(), self.pos_at) {
            (Some(p), Some(at)) if at.elapsed() < Duration::from_secs(3 * 3600) => Some(p.clone()),
            _ => None,
        }
    }

    fn fresh_adcode(&self) -> Option<String> {
        match (self.adcode.as_ref(), self.adcode_at) {
            (Some(a), Some(at)) if at.elapsed() < Duration::from_secs(24 * 3600) => Some(a.clone()),
            _ => None,
        }
    }

    /// 胶囊上的短文本，例如 "18°"
    pub fn short(&self) -> String {
        match &self.snap {
            Some(s) => format!("{}°", s.temp),
            None => "--".into(),
        }
    }
}

/// code → 中文描述
pub fn describe(code: u8) -> &'static str {
    match code {
        0 => "晴",
        1 => "晴间多云",
        2 => "多云",
        3 => "阴",
        45 | 48 => "雾",
        51 | 53 | 55 => "毛毛雨",
        56 | 57 => "冻雨",
        61 => "小雨",
        63 => "中雨",
        65 => "大雨",
        66 | 67 => "冻雨",
        71 => "小雪",
        73 => "中雪",
        75 => "大雪",
        77 => "雪粒",
        80..=82 => "阵雨",
        85 | 86 => "阵雪",
        95 => "雷阵雨",
        96 | 99 => "雷暴冰雹",
        _ => "多云",
    }
}

/// code → 符号图标（Segoe UI Symbol 覆盖这些码位）
/// 昼/夜不换字形（月牙在 15px 下看不清），由调用方用颜色区分。
pub fn glyph(code: u8, _day: bool) -> &'static str {
    match code {
        0 | 1 => "☀",
        2 | 3 | 45 | 48 => "☁",
        51..=57 | 61..=67 | 80..=82 => "☂",
        71..=77 | 85 | 86 => "❄",
        95..=99 => "⚡",
        _ => "☁",
    }
}

/// 高德的中文天气 → 归一化 code
fn amap_code(text: &str) -> u8 {
    let t = text.trim();
    match t {
        "晴" => 0,
        "少云" | "晴间多云" => 1,
        "多云" => 2,
        "阴" => 3,
        "阵雨" => 80,
        "雷阵雨" => 95,
        "雷阵雨伴有冰雹" => 96,
        "小雨" => 61,
        "中雨" => 63,
        "大雨" | "暴雨" | "大暴雨" | "特大暴雨" => 65,
        "雨夹雪" => 66,
        "小雪" => 71,
        "中雪" => 73,
        "大雪" | "暴雪" => 75,
        "雾" | "沙尘暴" | "扬沙" | "浮尘" => 45,
        "霾" => 48,
        _ => 3,
    }
}

// ---------- 网络 ----------

fn fetch(adcode: Option<String>, pos: Option<(f64, f64, String)>) -> Result<Snapshot, String> {
    // 数据源由设置页决定（weather_source）；高德失败仍回落 Open-Meteo，
    // 但指定 Open-Meteo 时不走高德
    let mut amap_err = String::new();
    if settings::weather_source() == settings::WeatherSource::Amap {
        let key = settings::amap_key();
        if !key.is_empty() {
            match fetch_amap(&key, adcode.as_deref()) {
                Ok(s) => return Ok(s),
                Err(e) => amap_err = e,
            }
        }
    }
    match fetch_open_meteo(pos) {
        Ok(s) => Ok(s),
        Err(e) => {
            if amap_err.is_empty() {
                Err(e)
            } else {
                Err(format!("高德: {amap_err}；Open-Meteo: {e}"))
            }
        }
    }
}

/// 高德：定城市 → base 实况 + all 四天预报
fn fetch_amap(key: &str, cached: Option<&str>) -> Result<Snapshot, String> {
    let mut place = String::new();
    let mut province = String::new();
    let adcode = match cached {
        Some(a) => a.to_string(),
        None => {
            let (code, city, prov) = amap_locate(key)?;
            place = city;
            province = prov;
            code
        }
    };

    let base = net::get(
        &format!("https://restapi.amap.com/v3/weather/weatherInfo?key={key}&city={adcode}&extensions=base"),
        8000,
    )?;
    let bj = Json::parse(&base).ok_or("高德实况响应解析失败")?;
    if bj.get("status").and_then(|v| v.str()) == Some("0") {
        return Err(bj.get("info").and_then(|v| v.str()).unwrap_or("高德实况失败").to_string());
    }
    let live = bj.get("lives").and_then(|v| v.at(0)).ok_or("高德未返回实况")?;

    let temp: i32 = live.get("temperature").and_then(|v| v.str()).unwrap_or("0").parse().unwrap_or(0);
    let weather = live.get("weather").and_then(|v| v.str()).unwrap_or("").to_string();
    let humidity: i32 = live.get("humidity").and_then(|v| v.str()).unwrap_or("0").parse().unwrap_or(0);
    let wind_dir = live.get("winddirection").and_then(|v| v.str()).unwrap_or("").to_string();
    let wind_power = live.get("windpower").and_then(|v| v.str()).unwrap_or("").to_string();
    let report = hhmm(live.get("reporttime").and_then(|v| v.str()).unwrap_or(""));
    if place.is_empty() {
        place = live.get("city").and_then(|v| v.str()).unwrap_or("").to_string();
    }
    if province.is_empty() {
        province = live.get("province").and_then(|v| v.str()).unwrap_or("").to_string();
    }

    let all = net::get(
        &format!("https://restapi.amap.com/v3/weather/weatherInfo?key={key}&city={adcode}&extensions=all"),
        8000,
    )?;
    let aj = Json::parse(&all).ok_or("高德预报响应解析失败")?;
    if aj.get("status").and_then(|v| v.str()) == Some("0") {
        return Err(aj.get("info").and_then(|v| v.str()).unwrap_or("高德预报失败").to_string());
    }
    let casts = aj
        .get("forecasts")
        .and_then(|v| v.at(0))
        .and_then(|f| f.get("casts"))
        .ok_or("高德未返回预报")?;

    let mut days = Vec::new();
    for i in 0..casts.len() {
        let Some(c) = casts.at(i) else { break };
        let dayw = c.get("dayweather").and_then(|v| v.str()).unwrap_or("").to_string();
        let nightw = c.get("nightweather").and_then(|v| v.str()).unwrap_or("").to_string();
        days.push(Day {
            label: day_label(i),
            date: c.get("date").and_then(|v| v.str()).unwrap_or("").to_string(),
            code: amap_code(&dayw),
            high: c.get("daytemp").and_then(|v| v.str()).unwrap_or("0").parse().unwrap_or(0),
            low: c.get("nighttemp").and_then(|v| v.str()).unwrap_or("0").parse().unwrap_or(0),
            rain: 0,
            day_weather: dayw,
            night_weather: nightw,
            day_wind: c.get("daywind").and_then(|v| v.str()).unwrap_or("").to_string(),
            night_wind: c.get("nightwind").and_then(|v| v.str()).unwrap_or("").to_string(),
            day_power: c.get("daypower").and_then(|v| v.str()).unwrap_or("").to_string(),
            night_power: c.get("nightpower").and_then(|v| v.str()).unwrap_or("").to_string(),
        });
    }
    if days.is_empty() {
        return Err("高德预报为空".into());
    }

    let code = amap_code(&weather);
    Ok(Snapshot {
        place: if place.is_empty() { province.clone() } else { place },
        province,
        condition: weather,
        temp,
        code,
        day: true,
        high: days[0].high,
        low: days[0].low,
        feels: None,
        humidity,
        wind: 0,
        wind_dir,
        wind_power,
        report,
        source: "高德",
        days,
        adcode,
        lat: 0.0,
        lon: 0.0,
    })
}

/// 高德定城市：先试 /v3/ip，拿不到 adcode 就用出口 IP 的城市名走地理编码
fn amap_locate(key: &str) -> Result<(String, String, String), String> {
    if let Ok(body) = net::get(&format!("https://restapi.amap.com/v3/ip?key={key}"), 8000) {
        if let Some(j) = Json::parse(&body) {
            let code = j.get("adcode").and_then(|v| v.str()).unwrap_or("");
            if !code.is_empty() {
                let city = j.get("city").and_then(|v| v.str()).unwrap_or("").to_string();
                let province = j.get("province").and_then(|v| v.str()).unwrap_or("").to_string();
                return Ok((code.to_string(), city, province));
            }
        }
    }

    let (_, _, guess) = locate()?;
    let body = net::get(
        &format!(
            "https://restapi.amap.com/v3/geocode/geo?key={key}&address={}",
            url_encode(&guess)
        ),
        8000,
    )?;
    let j = Json::parse(&body).ok_or("高德地理编码解析失败")?;
    let geo = j.get("geocodes").and_then(|g| g.at(0)).ok_or("高德地理编码无结果")?;
    let code = geo.get("adcode").and_then(|v| v.str()).unwrap_or("");
    if code.is_empty() {
        return Err("高德未返回城市编码".into());
    }
    let city = geo.get("city").and_then(|v| v.str()).unwrap_or("").to_string();
    let province = geo.get("province").and_then(|v| v.str()).unwrap_or("").to_string();
    Ok((code.to_string(), if city.is_empty() { guess } else { city }, province))
}

/// Open-Meteo 回退：出口 IP 定位 + 免 key 预报
fn fetch_open_meteo(cached: Option<(f64, f64, String)>) -> Result<Snapshot, String> {
    let (lat, lon, place) = match cached {
        Some(p) => p,
        None => locate()?,
    };
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,is_day,wind_speed_10m\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max\
         &timezone=auto&forecast_days=7"
    );
    let body = net::get(&url, 12000)?;
    parse_open_meteo(&body, lat, lon, &place)
}

/// 优先按配置城市解析坐标，留空则用出口 IP 定位
fn locate() -> Result<(f64, f64, String), String> {
    if !cfg::WEATHER_CITY.is_empty() {
        if let Some(p) = geocode(cfg::WEATHER_CITY) {
            return Ok(p);
        }
    }
    let body = net::get("https://ipwho.is/?lang=zh-CN", 8000)?;
    let j = Json::parse(&body).ok_or("定位响应解析失败")?;
    if j.get("success").and_then(|v| match v {
        Json::Bool(b) => Some(*b),
        _ => None,
    }) == Some(false)
    {
        return Err("定位服务拒绝请求".into());
    }
    let lat = j.get("latitude").and_then(|v| v.num()).ok_or("定位缺少纬度")?;
    let lon = j.get("longitude").and_then(|v| v.num()).ok_or("定位缺少经度")?;
    let city = j.get("city").and_then(|v| v.str()).unwrap_or("");
    let region = j.get("region").and_then(|v| v.str()).unwrap_or("");
    let place = if !city.is_empty() {
        city.to_string()
    } else if !region.is_empty() {
        region.to_string()
    } else {
        "本地".to_string()
    };
    Ok((lat, lon, place))
}

fn geocode(city: &str) -> Option<(f64, f64, String)> {
    let url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=10&language=zh&format=json",
        url_encode(city)
    );
    let body = net::get(&url, 8000).ok()?;
    let j = Json::parse(&body)?;
    let first = j.get("results")?.at(0)?;
    let lat = first.get("latitude")?.num()?;
    let lon = first.get("longitude")?.num()?;
    let name = first.get("name").and_then(|v| v.str()).unwrap_or(city);
    Some((lat, lon, name.to_string()))
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn parse_open_meteo(body: &str, lat: f64, lon: f64, place: &str) -> Result<Snapshot, String> {
    let j = Json::parse(body).ok_or("天气响应解析失败")?;
    if let Some(reason) = j.get("reason").and_then(|v| v.str()) {
        return Err(reason.to_string());
    }
    let cur = j.get("current").ok_or("天气响应缺少 current")?;
    let temp = cur.get("temperature_2m").and_then(|v| v.int()).ok_or("缺少温度")? as i32;
    let code = cur.get("weather_code").and_then(|v| v.int()).unwrap_or(3) as u8;
    let day = cur.get("is_day").and_then(|v| v.int()).unwrap_or(1) != 0;
    let humidity = cur.get("relative_humidity_2m").and_then(|v| v.int()).unwrap_or(0) as i32;
    let wind = cur.get("wind_speed_10m").and_then(|v| v.int()).unwrap_or(0) as i32;
    let feels = cur.get("apparent_temperature").and_then(|v| v.int()).map(|v| v as i32);

    let daily = j.get("daily");
    let highs = daily.and_then(|d| d.get("temperature_2m_max"));
    let lows = daily.and_then(|d| d.get("temperature_2m_min"));
    let codes = daily.and_then(|d| d.get("weather_code"));
    let rains = daily.and_then(|d| d.get("precipitation_probability_max"));
    let dates = daily.and_then(|d| d.get("time"));

    let mut days = Vec::new();
    for i in 0..7 {
        let (Some(h), Some(l)) = (
            highs.and_then(|v| v.at(i)).and_then(|v| v.int()),
            lows.and_then(|v| v.at(i)).and_then(|v| v.int()),
        ) else {
            break;
        };
        let c = codes.and_then(|v| v.at(i)).and_then(|v| v.int()).unwrap_or(3) as u8;
        let text = describe(c).to_string();
        days.push(Day {
            label: day_label(i),
            date: dates.and_then(|v| v.at(i)).and_then(|v| v.str()).unwrap_or("").to_string(),
            code: c,
            high: h as i32,
            low: l as i32,
            rain: rains.and_then(|v| v.at(i)).and_then(|v| v.int()).unwrap_or(0) as i32,
            day_weather: text.clone(),
            night_weather: text,
            day_wind: String::new(),
            night_wind: String::new(),
            day_power: String::new(),
            night_power: String::new(),
        });
    }
    if days.is_empty() {
        return Err("天气数据不完整".into());
    }

    Ok(Snapshot {
        place: place.to_string(),
        province: String::new(),
        condition: describe(code).to_string(),
        temp,
        code,
        day,
        high: days[0].high,
        low: days[0].low,
        feels,
        humidity,
        wind,
        wind_dir: String::new(),
        wind_power: String::new(),
        report: String::new(),
        source: "Open-Meteo",
        days,
        adcode: String::new(),
        lat,
        lon,
    })
}

/// "2026-11-19 17:30:00" → "17:30"
fn hhmm(s: &str) -> String {
    s.split(' ').nth(1).map(|t| t.chars().take(5).collect()).unwrap_or_default()
}

fn day_label(i: usize) -> String {
    match i {
        0 => "今天".into(),
        1 => "明天".into(),
        _ => {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            // 1970-01-01 是周四
            let wd = ((secs / 86400 + 4) % 7) as usize;
            const NAMES: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
            NAMES[(wd + i) % 7].to_string()
        }
    }
}
