// The Markdown renderer: examples of each construct; random input never
// breaks it; the 10x rule (a 6 MB novel is the worst case: 60 MB renders,
// and time grows linearly, including for the worst shapes: one 1.5 MB
// paragraph, long runs of markers that never close).
use crate::markdown::render;

fn html(md: &str) -> String {
    String::from_utf8(render(md.as_bytes()).bytes().clone()).unwrap()
}

pub fn run() {
    let mut fails = 0;
    let mut check = |name: &str, got: String, want: &str| {
        let ok = got == want;
        println!("{} {name}{}", if ok { "PASS" } else { "FAIL" }, if ok { String::new() } else { format!("\n   got  {got:?}\n   want {want:?}") });
        if !ok {
            fails += 1;
        }
    };
    check("paragraphs", html("One\ntwo\n\nThree"), "<p>One\ntwo</p><p>Three</p>");
    check("headings", html("# A\n## B\n### C"), "<h2>A</h2><h2>B</h2><h3>C</h3>");
    check("emphasis", html("a **b** *c* _d_ `e`"), "<p>a <strong>b</strong> <em>c</em> <em>d</em> <code>e</code></p>");
    check("a link", html("[here](https://example.com/x?y=1)"), "<p><a href=\"https://example.com/x?y=1\">here</a></p>");
    check("a bad link stays text", html("[x](javascript:alert(1))"), "<p>[x](javascript:alert(1))</p>");
    check("a protocol-relative link stays text", html("[x](//evil.com)"), "<p>[x](//evil.com)</p>");
    check("lists", html("- a\n- b\n\n1. c\n2. d"), "<ul><li>a</li><li>b</li></ul><ol><li>c</li><li>d</li></ol>");
    check("a quote", html("> q *e*\n> r"), "<blockquote><p>q <em>e</em>\nr</p></blockquote>");
    check("fenced code keeps its text", html("```\n<b>&x</b>\n```"), "<pre><code>&lt;b&gt;&amp;x&lt;/b&gt;</code></pre>");
    check("HTML in text is text", html("<script>alert('x')</script>"), "<p>&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;</p>");
    check("escapes", html("\\*not em\\*"), "<p>*not em*</p>");
    check("unclosed markers stay", html("**a *b `c [d"), "<p>**a *b `c [d</p>");
    check("a paragraph ends at a heading", html("para\n## H"), "<p>para</p><h2>H</h2>");
    check("CRLF", html("a\r\nb\r\n\r\nc"), "<p>a\nb</p><p>c</p>");
    check("words", crate::markdown::words("Hello, *world* — café 12".as_bytes()).to_string(), "4");

    // Random input: never a panic (the law holds whatever it is: proved).
    let mut seed = 99u64;
    let alphabet: &[u8] = b"ab *_`[]()#>-1.\\\n\n<&\"'https:/";
    for _ in 0..20_000 {
        let mut s = vec![];
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        for k in 0..(seed % 300) {
            s.push(alphabet[((seed >> (k % 50)) as usize + k as usize * 7) % alphabet.len()]);
        }
        let _ = render(&s);
    }
    println!("PASS 20,000 random inputs rendered");

    // 10x: novel-length. Time for 6 MB and 60 MB of prose; the worst shapes.
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
    for (name, one) in [("one 1.5 MB paragraph", "word ".repeat(300_000)), ("1.5 MB of unclosed markers", "*[`_**".repeat(250_000)), ("deep quotes", "> ".repeat(100_000) + "x")] {
        let ten = one.repeat(10);
        let t = std::time::Instant::now();
        render(one.as_bytes());
        let t1 = t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        render(ten.as_bytes());
        let t10 = t.elapsed().as_secs_f64();
        let ok = t10 / t1.max(1e-4) < 20.0 && t10 < 5.0;
        println!("{} {name}: {:.1} ms, 10x: {:.1} ms", if ok { "PASS" } else { "FAIL" }, t1 * 1000.0, t10 * 1000.0);
        if !ok {
            fails += 1;
        }
    }
    println!("\n{fails} failure(s)");
    if fails > 0 {
        std::process::exit(1);
    }
}
