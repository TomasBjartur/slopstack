// The browser's half (WebAssembly): the same CRDT (src/crdt.rs) and the
// same Markdown renderer with its proved markup builder (src/markdown.rs,
// src/html.rs, spec/markup.rs) as the server, compiled for wasm32 by
// tools/build_wasm.sh. The proofs are checked by Verus on the server build
// (tools/check.sh); this build erases them, as Verus's own compile does.
//
// The interface is numbers and one shared byte buffer each way (no
// bindings crate): JS writes input at input(n)'s pointer, calls a
// function, and reads output()/output_len(). One document per page.
// No unsafe: the buffers are ordinary Vecs; JS reaches them through the
// module's memory.
use std::cell::RefCell;

#[path = "../spec/markup.rs"]
pub mod spec_markup;
pub mod html;
pub mod md_tables;
pub mod markdown;
pub mod crdt;

use crdt::{Doc, Sink};

struct State {
    doc: Doc,
    input: Vec<u8>,
    output: Vec<u8>,
}

thread_local! {
    static S: RefCell<State> = RefCell::new(State { doc: Doc::new(), input: vec![], output: vec![] });
}

/// Changes as records for JS: position (u32), units deleted (u32), byte
/// length (u32), UTF-8 text.
struct Events<'a>(&'a mut Vec<u8>, u32);

impl Sink for Events<'_> {
    fn change(&mut self, pos: u64, del: u64, ins: &str) {
        self.1 += 1;
        if self.1 > MAX_EVENTS {
            return;
        }
        self.0.extend_from_slice(&(pos as u32).to_le_bytes());
        self.0.extend_from_slice(&(del as u32).to_le_bytes());
        self.0.extend_from_slice(&(ins.len() as u32).to_le_bytes());
        self.0.extend_from_slice(ins.as_bytes());
    }
}

/// Past this many changes in one batch, JS is told to take the whole text
/// instead (cheaper than replaying them one by one).
const MAX_EVENTS: u32 = 2000;

/// A buffer of n bytes for JS to fill; its address.
#[no_mangle]
pub extern "C" fn input(n: u32) -> *mut u8 {
    S.with(|s| {
        let mut s = s.borrow_mut();
        s.input.clear();
        s.input.resize(n as usize, 0);
        s.input.as_mut_ptr()
    })
}

#[no_mangle]
pub extern "C" fn output() -> *const u8 {
    S.with(|s| s.borrow().output.as_ptr())
}

#[no_mangle]
pub extern "C" fn output_len() -> u32 {
    S.with(|s| s.borrow().output.len() as u32)
}

fn code(b: crdt::Bad) -> i32 {
    match b {
        crdt::Bad::Format => -1,
        crdt::Bad::Id => -2,
        crdt::Bad::Missing => -3,
        crdt::Bad::Position => -4,
    }
}

/// A fresh, empty document.
#[no_mangle]
pub extern "C" fn reset() {
    S.with(|s| s.borrow_mut().doc = Doc::new());
}

/// Loads a snapshot (the input): 0, or a negative error.
#[no_mangle]
pub extern "C" fn load() -> i32 {
    S.with(|s| {
        let mut s = s.borrow_mut();
        match Doc::load(&s.input) {
            Ok(d) => {
                s.doc = d;
                0
            }
            Err(e) => code(e),
        }
    })
}

/// A local edit: del units at pos replaced by the input (UTF-8), as rep.
/// The output is its operations. 0, or a negative error.
#[no_mangle]
pub extern "C" fn edit(rep: u32, pos: u32, del: u32) -> i32 {
    S.with(|s| {
        let s = &mut *s.borrow_mut();
        let Ok(ins) = std::str::from_utf8(&s.input) else { return code(crdt::Bad::Format) };
        s.output.clear();
        match s.doc.edit(rep, pos as u64, del as u64, ins, &mut s.output) {
            Ok(()) => 0,
            Err(e) => code(e),
        }
    })
}

/// Applies a batch (the input). The output is the changes, as records.
/// Answers how many changes (more than MAX_EVENTS: the records stop; take
/// the whole text), or a negative error (the document may be partly
/// changed: reload it).
#[no_mangle]
pub extern "C" fn apply() -> i32 {
    S.with(|s| {
        let s = &mut *s.borrow_mut();
        s.output.clear();
        let mut ev = Events(&mut s.output, 0);
        match s.doc.apply_batch(&s.input, &mut ev) {
            Ok(_) => ev.1 as i32,
            Err(e) => code(e),
        }
    })
}

/// The text into the output (UTF-8); its byte length.
#[no_mangle]
pub extern "C" fn text() -> u32 {
    S.with(|s| {
        let s = &mut *s.borrow_mut();
        s.output = s.doc.text().into_bytes();
        s.output.len() as u32
    })
}

/// Live length in UTF-16 units.
#[no_mangle]
pub extern "C" fn len16() -> f64 {
    S.with(|s| s.borrow().doc.len16() as f64)
}

/// Markdown (the input) rendered to allowed markup (the output); its
/// length. The proved builder (src/html.rs) is what makes it: the same
/// HTML the server publishes.
#[no_mangle]
pub extern "C" fn render() -> u32 {
    S.with(|s| {
        let s = &mut *s.borrow_mut();
        let m = markdown::render(&s.input);
        s.output.clear();
        s.output.extend_from_slice(m.bytes());
        s.output.len() as u32
    })
}
