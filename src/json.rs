//! 极小 JSON 解析器：只服务本项目自己的接口数据，不做完整校验、不追求报错位置。
//! 支持 object / array / string / number / bool / null，数字统一 f64。

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn parse(s: &str) -> Option<Json> {
        let mut p = Parser { b: s.as_bytes(), i: 0 };
        p.ws();
        p.value()
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn at(&self, i: usize) -> Option<&Json> {
        match self {
            Json::Arr(a) => a.get(i),
            _ => None,
        }
    }

    pub fn num(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// 取整（四舍五入），用于温度、湿度等
    pub fn int(&self) -> Option<i64> {
        self.num().map(|n| n.round() as i64)
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(a) => a,
            _ => &[],
        }
    }

    /// 数组/对象的成员数（其余类型为 0）
    pub fn len(&self) -> usize {
        match self {
            Json::Arr(a) => a.len(),
            Json::Obj(m) => m.len(),
            _ => 0,
        }
    }

    /// 沿路径取值：`root.path("current.temperature_2m")`
    pub fn path(&self, dotted: &str) -> Option<&Json> {
        let mut cur = self;
        for seg in dotted.split('.') {
            cur = match seg.parse::<usize>() {
                Ok(i) => cur.at(i)?,
                Err(_) => cur.get(seg)?,
            };
        }
        Some(cur)
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn value(&mut self) -> Option<Json> {
        self.ws();
        match self.peek()? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string().map(Json::Str),
            b't' => self.literal("true").map(|_| Json::Bool(true)),
            b'f' => self.literal("false").map(|_| Json::Bool(false)),
            b'n' => self.literal("null").map(|_| Json::Null),
            _ => self.number(),
        }
    }

    fn literal(&mut self, lit: &str) -> Option<()> {
        if self.b.get(self.i..self.i + lit.len())? == lit.as_bytes() {
            self.i += lit.len();
            Some(())
        } else {
            None
        }
    }

    fn object(&mut self) -> Option<Json> {
        self.i += 1; // {
        let mut out = Vec::new();
        self.ws();
        if self.peek()? == b'}' {
            self.i += 1;
            return Some(Json::Obj(out));
        }
        loop {
            self.ws();
            if self.peek()? != b'"' {
                return None;
            }
            let key = self.string()?;
            self.ws();
            if self.peek()? != b':' {
                return None;
            }
            self.i += 1;
            let v = self.value()?;
            out.push((key, v));
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Some(Json::Obj(out));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self) -> Option<Json> {
        self.i += 1; // [
        let mut out = Vec::new();
        self.ws();
        if self.peek()? == b']' {
            self.i += 1;
            return Some(Json::Arr(out));
        }
        loop {
            out.push(self.value()?);
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Some(Json::Arr(out));
                }
                _ => return None,
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        self.i += 1; // "
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let hex = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
                            let mut cp = u32::from_str_radix(hex, 16).ok()?;
                            self.i += 4;
                            // 代理对
                            if (0xD800..0xDC00).contains(&cp) {
                                if self.b.get(self.i..self.i + 2)? == b"\\u" {
                                    let hex2 =
                                        std::str::from_utf8(self.b.get(self.i + 2..self.i + 6)?).ok()?;
                                    let lo = u32::from_str_radix(hex2, 16).ok()?;
                                    self.i += 6;
                                    cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                }
                            }
                            let ch = char::from_u32(cp)?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        other => out.push(other),
                    }
                }
                _ => out.push(c),
            }
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E') {
                self.i += 1;
            } else {
                break;
            }
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).ok()?;
        s.parse::<f64>().ok().map(Json::Num)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_weather_shape() {
        let raw = r#"{"current":{"temperature_2m":18.6,"weather_code":3,"is_day":1},
            "daily":{"temperature_2m_max":[21.2,-1],"temperature_2m_min":[9.1,3]}}"#;
        let j = Json::parse(raw).unwrap();
        assert_eq!(j.path("current.temperature_2m").and_then(|v| v.int()), Some(19));
        assert_eq!(j.path("daily.temperature_2m_max.1").and_then(|v| v.int()), Some(-1));
        assert_eq!(j.path("current.weather_code").and_then(|v| v.int()), Some(3));
    }

    #[test]
    fn handles_escapes_and_unicode() {
        let j = Json::parse(r#"{"city":"\u5e7f\u5dde","note":"a\"b"}"#).unwrap();
        assert_eq!(j.get("city").and_then(|v| v.str()), Some("广州"));
        assert_eq!(j.get("note").and_then(|v| v.str()), Some("a\"b"));
    }
}
