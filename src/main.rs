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
pub mod crdt;
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
pub mod docs;
pub mod sim;
pub mod tests {
    pub mod crdt;
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
            "crdt" => tests::crdt::run(),
            "crdt-lean" => tests::crdt::lean_case(args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1)),
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
            "render" => {
                // Markdown on stdin, its HTML on stdout (tests/wasm_test.mjs).
                use std::io::{Read, Write};
                let mut md = vec![];
                std::io::stdin().read_to_end(&mut md).expect("stdin");
                std::io::stdout().write_all(markdown::render(&md).bytes()).expect("stdout");
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
    let path = std::env::var("BLOG_DB").unwrap_or_else(|_| "blog.db".into());
    // WORKER PROCESSES (BLOG_WORKERS, default 1): one event loop each, on
    // one shared listening socket; each opens its own database connection
    // (SQLite in WAL mode: one writer at a time, many readers). What they
    // share is only the database: caches catch up with it (src/docs.rs) or
    // are keyed by what it says (post pages by updated_ms).
    let workers: u32 = std::env::var("BLOG_WORKERS").ok().and_then(|w| w.parse().ok()).unwrap_or(1).clamp(1, 64);
    // The database is migrated once, before anyone connects.
    drop(db::Store::open(&path).unwrap_or_else(|e| panic!("cannot open {path}: {e:?}")));
    let listener = sys::linux::listen_loopback(port, 4096).unwrap_or_else(|e| panic!("cannot listen on {port}: errno {e}"));
    eprintln!("listening on 127.0.0.1:{port} ({workers} worker(s))");
    if workers == 1 {
        serve(listener, false, &path, port);
    }
    let me = std::process::id() as i32;
    let spawn = || match sys::linux::fork_process() {
        Ok(0) => {
            sys::linux::die_with_parent(me);
            serve(listener, true, &path, port);
        }
        Ok(pid) => pid,
        Err(e) => panic!("fork: errno {e}"),
    };
    let mut pids: Vec<i32> = (0..workers).map(|_| spawn()).collect();
    // Supervision: a worker that ends is replaced (after a pause, so a
    // worker that dies at once does not spin).
    loop {
        let Ok(pid) = sys::linux::wait_child() else { continue };
        if let Some(i) = pids.iter().position(|&p| p == pid) {
            eprintln!("worker {pid} ended; starting another");
            std::thread::sleep(std::time::Duration::from_millis(500));
            pids[i] = spawn();
        }
    }
}

fn serve(listener: sys::linux::fd, shared: bool, path: &str, port: u16) -> ! {
    let st = db::Store::open(path).unwrap_or_else(|e| panic!("cannot open {path}: {e:?}"));
    let site = site::Site::new(st, site::Conf::from_env(port));
    let io = io::LinuxIo::on(listener, shared).unwrap_or_else(|e| panic!("epoll: errno {e}"));
    let mut s = server::Server::new(io, site);
    loop {
        s.turn(1000);
    }
}
