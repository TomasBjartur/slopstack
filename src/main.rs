// The server. See DESIGN.md. Proved modules are inside verus! blocks; the
// rest is ordinary Rust, tested (each file says how).
use vstd::prelude::*;

pub mod limits;
#[path = "../spec/http.rs"]
pub mod spec_http;
pub mod http;
pub mod sys {
    pub mod linux;
}
pub mod io;
pub mod server;
pub mod app;
pub mod sim;

verus! {
#[verifier::external_body]
fn main() {
    run();
}
}

fn run() {
    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8190);
    let io = io::LinuxIo::new(port).unwrap_or_else(|e| panic!("cannot listen on {port}: errno {e}"));
    eprintln!("listening on 127.0.0.1:{port}");
    let mut s = server::Server::new(io, app::Site);
    loop {
        s.turn(1000);
    }
}
