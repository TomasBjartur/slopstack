// The server. See DESIGN.md.
use vstd::prelude::*;

pub mod limits;
#[path = "../spec/http.rs"]
pub mod spec_http;
pub mod http;

verus! {
fn main() {}
}
