// JSON (RFC 8259), strict and bounded: for passkey data (clientDataJSON)
// and tests. Nesting at most DEPTH; strings must be valid UTF-8 with valid
// escapes (a lone surrogate is refused); numbers as f64; nothing after the
// value but whitespace.
const DEPTH: u32 = 32;

#[derive(Debug, PartialEq, Clone)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        if let Json::Str(s) = self { Some(s) } else { None }
    }
    pub fn num(&self) -> Option<f64> {
        if let Json::Num(n) = self { Some(*n) } else { None }
    }
}

pub fn parse(b: &[u8]) -> Option<Json> {
    let mut p = P { b, i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i == b.len() { Some(v) } else { None }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn lit(&mut self, s: &[u8]) -> bool {
        if self.b[self.i..].starts_with(s) {
            self.i += s.len();
            true
        } else {
            false
        }
    }
    fn value(&mut self, depth: u32) -> Option<Json> {
        if depth > DEPTH {
            return None;
        }
        self.ws();
        match *self.b.get(self.i)? {
            b'n' => self.lit(b"null").then_some(Json::Null),
            b't' => self.lit(b"true").then_some(Json::Bool(true)),
            b'f' => self.lit(b"false").then_some(Json::Bool(false)),
            b'"' => self.string().map(Json::Str),
            b'[' => {
                self.i += 1;
                let mut v = vec![];
                self.ws();
                if self.eat(b']') {
                    return Some(Json::Arr(v));
                }
                loop {
                    v.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat(b']') {
                        return Some(Json::Arr(v));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'{' => {
                self.i += 1;
                let mut v = vec![];
                self.ws();
                if self.eat(b'}') {
                    return Some(Json::Obj(v));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return None;
                    }
                    let k = self.string()?;
                    self.ws();
                    if !self.eat(b':') {
                        return None;
                    }
                    v.push((k, self.value(depth + 1)?));
                    self.ws();
                    if self.eat(b'}') {
                        return Some(Json::Obj(v));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }
    fn number(&mut self) -> Option<Json> {
        let s = self.i;
        self.eat(b'-');
        if self.eat(b'0') {
        } else {
            let d = self.i;
            while self.b.get(self.i).map_or(false, |c| c.is_ascii_digit()) {
                self.i += 1;
            }
            if self.i == d {
                return None;
            }
        }
        if self.eat(b'.') {
            let d = self.i;
            while self.b.get(self.i).map_or(false, |c| c.is_ascii_digit()) {
                self.i += 1;
            }
            if self.i == d {
                return None;
            }
        }
        if self.eat(b'e') || self.eat(b'E') {
            let _ = self.eat(b'+') || self.eat(b'-');
            let d = self.i;
            while self.b.get(self.i).map_or(false, |c| c.is_ascii_digit()) {
                self.i += 1;
            }
            if self.i == d {
                return None;
            }
        }
        std::str::from_utf8(&self.b[s..self.i]).ok()?.parse().ok().map(Json::Num)
    }
    fn hex4(&mut self) -> Option<u32> {
        let h = self.b.get(self.i..self.i + 4)?;
        self.i += 4;
        u32::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()
    }
    fn string(&mut self) -> Option<String> {
        self.i += 1; // "
        let mut out: Vec<u8> = vec![];
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let a = self.hex4()?;
                            let cp = if (0xd800..0xdc00).contains(&a) {
                                if !self.lit(b"\\u") {
                                    return None;
                                }
                                let b2 = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&b2) {
                                    return None;
                                }
                                0x10000 + ((a - 0xd800) << 10) + (b2 - 0xdc00)
                            } else if (0xdc00..0xe000).contains(&a) {
                                return None;
                            } else {
                                a
                            };
                            char::from_u32(cp)?
                        }
                        _ => return None,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                0..=0x1f => return None,
                _ => out.push(c),
            }
        }
    }
}
