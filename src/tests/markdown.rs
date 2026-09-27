// The Markdown renderer against every example of the CommonMark 0.31.2
// spec (tests/commonmark/spec.json): the HTML must be exactly the spec's,
// except where the markup law (spec/markup.rs) requires otherwise: raw
// HTML is shown as text; a URL with a scheme other than http, https or
// mailto is not a link. Those examples are listed as deviations, with the
// reason; every other example must pass.
// Then: safety cases (what an attacker would write), random input (never a
// panic), and the 10x rule (a 6 MB novel is the worst case: 60 MB, and the
// worst shapes, grow linearly).
use crate::json::{self, Json};
use crate::markdown::render;

fn html(md: &str) -> String {
    String::from_utf8(render(md.as_bytes()).bytes().clone()).unwrap()
}

/// Why the law makes this example differ from the spec, if it does.
fn deviation(md: &str, want: &str) -> Option<&'static str> {
    // A tag the writer wrote, passed through by the spec (raw HTML).
    let mut k = 0;
    while let Some(o) = want[k..].find('<') {
        let rest = &want[k + o..];
        let tag = &rest[..rest.find('>').map_or(rest.len(), |e| e + 1)];
        if tag.len() > 2 && md.contains(tag) {
            return Some("raw HTML (shown as text)");
        }
        k += o + 1;
    }
    let allowed = ["p", "h1", "h2", "h3", "h4", "h5", "h6", "strong", "em", "code", "pre", "blockquote", "ul", "ol", "li", "br", "hr", "a", "img"];
    let b = want.as_bytes();
    let mut i = 0;
    while let Some(k) = want[i..].find('<') {
        let at = i + k + 1;
        let name: String = want[at..].trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        if !allowed.contains(&name.to_ascii_lowercase().as_str()) {
            return Some("raw HTML (shown as text)");
        }
        i = at;
        let _ = b;
    }
    for attr in ["href=\"", "src=\""] {
        let mut j = 0;
        while let Some(k) = want[j..].find(attr) {
            let u = &want[j + k + attr.len()..];
            let url = &u[..u.find('"').unwrap_or(u.len())];
            let sch: String = url.chars().take_while(|c| c.is_ascii_alphanumeric() || "+-.".contains(*c)).collect();
            if url[sch.len()..].starts_with(':') && url.chars().next().map_or(false, |c| c.is_ascii_alphabetic()) {
                if !["http", "https", "mailto"].contains(&sch.to_ascii_lowercase().as_str()) {
                    return Some("a URL scheme the law does not allow");
                }
            }
            if url.contains('\\') || url.contains('`') {
                return Some("a URL byte the law does not allow");
            }
            j += k + attr.len();
        }
    }
    let _ = md;
    None
}

/// A tag (between < and >) with no script: not a script element, no on*
/// attribute, no href or src with a script or data URL.
fn tag_inert(t: &str) -> bool {
    let name: String = t.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    if name == "script" || name == "iframe" || name == "object" {
        return false;
    }
    // Attributes, outside quotes.
    let b = t.as_bytes();
    let mut i = name.len();
    while i < b.len() {
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        let s = i;
        while i < b.len() && b[i] != b'=' && b[i] != b' ' {
            i += 1;
        }
        let attr = &t[s..i];
        let mut val = "";
        if i < b.len() && b[i] == b'=' {
            i += 1;
            if i < b.len() && b[i] == b'"' {
                let e = t[i + 1..].find('"').map_or(b.len(), |e| i + 1 + e);
                val = &t[i + 1..e];
                i = e + 1;
            } else {
                return false; // (we only write quoted values)
            }
        }
        if attr.starts_with("on") || ((attr == "href" || attr == "src") && (val.starts_with("javascript:") || val.starts_with("data:") || val.starts_with("vbscript:"))) {
            return false;
        }
    }
    true
}

pub fn run() {
    let mut fails = 0;
    let spec = std::fs::read("tests/commonmark/spec.json").expect("tests/commonmark/spec.json (run from the repository)");
    let Some(Json::Arr(examples)) = json::parse(&spec) else { panic!("spec.json") };
    let (mut pass, mut dev, mut fail) = (0, 0, 0);
    let mut by_reason = std::collections::BTreeMap::new();
    let mut failed_sections = std::collections::BTreeMap::new();
    for ex in &examples {
        let md = ex.get("markdown").and_then(|v| v.str()).unwrap();
        let want = ex.get("html").and_then(|v| v.str()).unwrap();
        let n = ex.get("example").and_then(|v| v.num()).unwrap() as u32;
        let section = ex.get("section").and_then(|v| v.str()).unwrap();
        let got = html(md);
        if got == want {
            pass += 1;
        } else if let Some(r) = deviation(md, want) {
            if std::env::var("MD_DEVIATIONS").is_ok() {
                println!("DEVIATION example {n} ({r})\n  md   {md:?}\n  got  {got:?}\n  want {want:?}");
            }
            dev += 1;
            *by_reason.entry(r).or_insert(0) += 1;
        } else {
            fail += 1;
            *failed_sections.entry(section.to_string()).or_insert(0) += 1;
            if std::env::var("MD_VERBOSE").is_ok() || fail <= 5 {
                println!("FAIL example {n} ({section})\n  md   {md:?}\n  got  {got:?}\n  want {want:?}");
            }
        }
    }
    println!("CommonMark 0.31.2: {} examples: {pass} pass, {dev} differ as the law requires, {fail} fail", examples.len());
    for (r, k) in &by_reason {
        println!("  deviation: {k} x {r}");
    }
    for (s, k) in &failed_sections {
        println!("  failing: {k} in {s}");
    }
    if fail > 0 {
        fails += 1;
    }

    // Safety: what an attacker would write.
    let mut check = |name: &str, got: String, ok: bool| {
        println!("{} {name}{}", if ok { "PASS" } else { "FAIL" }, if ok { String::new() } else { format!(": {got:?}") });
        if !ok {
            fails += 1;
        }
    };
    let bad = [
        "<script>alert(1)</script>",
        "[x](javascript:alert(1))",
        "[x](JaVaScRiPt:alert(1))",
        "[x](java&#9;script:alert(1))",
        "[x](<javascript:alert(1)>)",
        "![x](data:text/html,<script>alert(1)</script>)",
        "<javascript:alert(1)>",
        "[x]: javascript:alert(1)\n\n[x]",
        "[x](\"onmouseover=alert(1))",
        "`<img src=x onerror=alert(1)>`",
        "<img src=x onerror=alert(1)>",
        "[a](http://x \"t\\\" onmouseover=\\\"alert(1)\")",
    ];
    for b in bad {
        let h = html(b);
        // Inert: no tag carries a script (text that reads like one is text).
        let lower = h.to_ascii_lowercase();
        let tags: Vec<&str> = lower.split('<').skip(1).map(|t| t.split('>').next().unwrap_or("")).collect();
        let ok = tags.iter().all(|t| tag_inert(t));
        check(&format!("an attack is inert: {b:?}"), h, ok);
    }
    check("a safe link works", html("[here](https://example.com/a?b=1&c=2)"), html("[here](https://example.com/a?b=1&c=2)") == "<p><a href=\"https://example.com/a?b=1&amp;c=2\">here</a></p>\n");

    // Random input: never a panic.
    let mut seed = 99u64;
    let alphabet: &[u8] = b"ab *_`[]()#>-1.\\\n\n<&\"'https:/!\t~=";
    for _ in 0..30_000 {
        let mut s = vec![];
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut x = seed;
        for _ in 0..(seed % 300) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            s.push(alphabet[(x % alphabet.len() as u64) as usize]);
        }
        let _ = render(&s);
    }
    println!("PASS 30,000 random inputs rendered");

    // 10x: novel-length, and the worst shapes; time must grow linearly.
    let para = "The river of long evenings carries *small boats* past old walls where people talk about **books** and [maps](https://example.com). ".repeat(8) + "\n\n";
    let six = para.repeat(6_000_000 / para.len());
    let sixty = six.repeat(10);
    let t = std::time::Instant::now();
    let a = render(six.as_bytes()).len();
    let t6 = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    let b = render(sixty.as_bytes()).len();
    let t60 = t.elapsed().as_secs_f64();
    let ok = b > a * 9 && t60 / t6 < 15.0;
    println!("{} a novel: 6 MB in {:.0} ms ({:.0} MB/s), 60 MB in {:.0} ms ({:.1}x)", if ok { "PASS" } else { "FAIL" }, t6 * 1000.0, 6.0 / t6, t60 * 1000.0, t60 / t6);
    if !ok {
        fails += 1;
    }
    // Each shape made at N and at 10N (not repeated: a repeat can change
    // what the text is, as more references after text become links).
    let shapes: Vec<(&str, Box<dyn Fn(usize) -> String>)> = vec![
        ("one 1.5 MB paragraph", Box::new(|k| "word ".repeat(300_000 * k))),
        ("unclosed emphasis", Box::new(|k| "*a _b ".repeat(250_000 * k))),
        ("unclosed brackets", Box::new(|k| "[a ".repeat(500_000 * k))),
        ("unclosed backticks", Box::new(|k| "`a ``b ".repeat(200_000 * k))),
        ("nested quotes", Box::new(|k| "> ".repeat(20_000 * k) + "x")),
        ("nested lists", Box::new(|k| (0..2000 * k).map(|i| " ".repeat((i % 64) * 2) + "- x\n").collect::<String>())),
        ("many references", Box::new(|k| (0..30_000 * k).map(|i| format!("[r{i}]: /u{i}\n")).collect::<String>() + "[r1] [r29999]")),
        ("many links", Box::new(|k| "[a](/b) ".repeat(100_000 * k))),
    ];
    for (name, make) in shapes {
        let (one, ten) = (make(1), make(10));
        let best = |s: &str| (0..3).map(|_| {
            let t = std::time::Instant::now();
            render(s.as_bytes());
            t.elapsed().as_secs_f64()
        }).fold(f64::MAX, f64::min);
        let (t1, t10) = (best(&one), best(&ten));
        // Linear: about 10x, with room for the memory effects of a 10x
        // larger input (out of the caches); quadratic would be ~100x.
        let ok = t10 / t1.max(1e-3) < 20.0 && t10 < 10.0;
        println!("{} {name}: {:.1} ms, 10x: {:.1} ms ({:.1}x)", if ok { "PASS" } else { "FAIL" }, t1 * 1000.0, t10 * 1000.0, t10 / t1.max(1e-3));
        if !ok {
            fails += 1;
        }
    }
    println!("\n{fails} failure(s)");
    if fails > 0 {
        std::process::exit(1);
    }
}
