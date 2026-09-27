// The server. See DESIGN.md. Proved modules are inside verus! blocks; the
// rest is ordinary Rust, tested (each file says how).
use vstd::prelude::*;

pub mod limits;
#[path = "../spec/http.rs"]
pub mod spec_http;
pub mod http;
#[path = "../spec/authz.rs"]
pub mod spec_authz;
pub mod authz;
#[path = "../spec/markup.rs"]
pub mod spec_markup;
pub mod html;
#[path = "../spec/webauthn.rs"]
pub mod spec_webauthn;
pub mod webauthn;
pub mod markdown;
pub mod md_tables;
pub mod json;
pub mod sys {
    pub mod linux;
    pub mod sqlite;
    pub mod crypto;
}
pub mod db;
pub mod resp;
pub mod form;
pub mod io;
pub mod server;
pub mod app;
pub mod assets;
pub mod pages;
pub mod site;
pub mod sim;
pub mod tests {
    pub mod crypto;
    pub mod db;
    pub mod http;
    pub mod markdown;
    pub mod sim;
}

verus! {
#[verifier::external_body]
fn main() {
    run();
}
}

fn run() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "test" {
        match args[2].as_str() {
            "crypto" => tests::crypto::run(),
            "db" => tests::db::run(),
            "http" => tests::http::run(),
            "markdown" => tests::markdown::run(),
            "mdrefs" => {
                for n in [30_000, 100_000, 300_000] {
                    let md: String = (0..n).map(|i| format!("[r{i}]: /u{i}\n")).collect::<String>() + "[r1] [r29999]";
                    let t = std::time::Instant::now();
                    markdown::render(md.as_bytes());
                    eprintln!("{n} refs: {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
                    let md2 = md.repeat(3);
                    let t = std::time::Instant::now();
                    markdown::render(md2.as_bytes());
                    eprintln!("{n} refs x3 (then text): {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
                }
            }
            "mdbench" => {
                let para = "The river of long evenings carries *small boats* past old walls where people talk about **books** and [maps](https://example.com). ".repeat(8) + "\n\n";
                let six = para.repeat(6_000_000 / para.len());
                for _ in 0..5 {
                    let t = std::time::Instant::now();
                    let n = markdown::render(six.as_bytes()).len();
                    eprintln!("{n} bytes in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
                }
            }
            "sim" => tests::sim::run(),
            t => panic!("no test {t}"),
        }
        return;
    }
    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8190);
    // The database first (migrated): nobody connects before it is ready.
    let path = std::env::var("BLOG_DB").unwrap_or_else(|_| "blog.db".into());
    let st = db::Store::open(&path).unwrap_or_else(|e| panic!("cannot open {path}: {e:?}"));
    let site = site::Site::new(st, site::Conf::from_env(port));
    let io = io::LinuxIo::new(port).unwrap_or_else(|e| panic!("cannot listen on {port}: errno {e}"));
    eprintln!("listening on 127.0.0.1:{port}");
    let mut s = server::Server::new(io, site);
    loop {
        s.turn(1000);
    }
}
