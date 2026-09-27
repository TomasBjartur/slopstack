// application/x-www-form-urlencoded bodies and query strings: strict
// (valid percent-encoding, UTF-8 text, at most FIELDS_MAX fields; a name
// repeated is an error: forms never repeat a field, so a repeat is a
// confusion to refuse, not to guess).
const FIELDS_MAX: usize = 64;

pub struct Form {
    fields: Vec<(String, String)>,
}

fn decode(s: &[u8]) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let h = |c: u8| (c as char).to_digit(16);
                let (a, b) = (h(*s.get(i + 1)?)?, h(*s.get(i + 2)?)?);
                out.push((a * 16 + b) as u8);
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

impl Form {
    pub fn parse(b: &[u8]) -> Option<Form> {
        let mut fields: Vec<(String, String)> = vec![];
        if b.is_empty() {
            return Some(Form { fields });
        }
        for part in b.split(|&c| c == b'&') {
            if part.is_empty() {
                continue;
            }
            let (k, v) = match part.iter().position(|&c| c == b'=') {
                Some(e) => (&part[..e], &part[e + 1..]),
                None => (part, &b""[..]),
            };
            let k = decode(k)?;
            if fields.len() == FIELDS_MAX || fields.iter().any(|(n, _)| *n == k) {
                return None;
            }
            fields.push((k, decode(v)?));
        }
        Some(Form { fields })
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// A field as text: trimmed, 1..=max characters, no control characters
    /// (line breaks too, unless lines).
    pub fn text(&self, name: &str, max: usize, lines: bool) -> Option<String> {
        let v = self.get(name)?.trim();
        let n = v.chars().count();
        if n == 0 || n > max || v.chars().any(|c| c.is_control() && !(lines && (c == '\n' || c == '\r' || c == '\t'))) {
            return None;
        }
        Some(v.to_string())
    }

    pub fn num(&self, name: &str) -> Option<u64> {
        let v = self.get(name)?;
        if v.is_empty() || v.len() > 15 || !v.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        v.parse().ok()
    }
}

/// Percent-encodes a value for a form body (the inverse of parse).
pub fn encode(v: &str) -> String {
    const H: &[u8; 16] = b"0123456789ABCDEF";
    let mut s = String::with_capacity(v.len());
    for &c in v.as_bytes() {
        if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.' || c == b'~' {
            s.push(c as char);
        } else {
            s.push('%');
            s.push(H[(c >> 4) as usize] as char);
            s.push(H[(c & 15) as usize] as char);
        }
    }
    s
}

/// A slug from a title: lower-case ASCII letters and digits, words joined
/// by '-', at most max bytes; "" if nothing is left.
pub fn slugify(title: &str, max: usize) -> String {
    let mut s = String::new();
    let mut dash = false;
    for c in title.chars().flat_map(|c| c.to_lowercase()) {
        let c = match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            if dash && !s.is_empty() {
                s.push('-');
            }
            dash = false;
            if s.len() < max {
                s.push(c);
            }
        } else {
            dash = true;
        }
    }
    while s.len() > max || s.ends_with('-') {
        s.pop();
    }
    s
}

/// An email address we will send to: local@domain, the local part of
/// letters, digits and !#$%&'*+/=?^_`{|}~.- (no quotes, spaces, brackets
/// or line breaks: nothing that could change a mail header), the domain
/// dot-separated labels of letters, digits and -, with at least one dot.
pub fn email_ok(e: &str) -> bool {
    let Some((local, domain)) = e.split_once('@') else { return false };
    let local_ok = |c: u8| c.is_ascii_alphanumeric() || b"!#$%&'*+/=?^_`{|}~.-".contains(&c);
    e.len() <= 254
        && !local.is_empty()
        && local.len() <= 64
        && local.bytes().all(local_ok)
        && domain.contains('.')
        && domain.split('.').all(|l| !l.is_empty() && l.len() <= 63 && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
}
