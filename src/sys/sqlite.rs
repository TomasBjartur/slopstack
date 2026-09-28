// SQLite (vendor/sqlite), declared by hand: the functions we use, and a
// small safe layer over them. Statements are prepared once, at start, and
// named by index; a query binds values, hands each row to a closure (which
// cannot keep it: the row lives only during the step), and always resets.
// A deadline stops a query that runs too long (the progress handler).
use std::ffi::c_void;
use std::os::raw::{c_char, c_int};

#[repr(C)]
pub struct sqlite3 {
    _p: [u8; 0],
}
#[repr(C)]
pub struct sqlite3_stmt {
    _p: [u8; 0],
}

extern "C" {
    fn sqlite3_open_v2(path: *const c_char, db: *mut *mut sqlite3, flags: c_int, vfs: *const c_char) -> c_int;
    fn sqlite3_close_v2(db: *mut sqlite3) -> c_int;
    fn sqlite3_exec(db: *mut sqlite3, sql: *const c_char, cb: *const c_void, arg: *mut c_void, err: *mut *mut c_char) -> c_int;
    fn sqlite3_prepare_v3(db: *mut sqlite3, sql: *const c_char, n: c_int, flags: u32, st: *mut *mut sqlite3_stmt, tail: *mut *const c_char) -> c_int;
    fn sqlite3_step(st: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_reset(st: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_stmt_status(st: *mut sqlite3_stmt, op: c_int, reset: c_int) -> c_int;
    fn sqlite3_clear_bindings(st: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_finalize(st: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_bind_int64(st: *mut sqlite3_stmt, i: c_int, v: i64) -> c_int;
    fn sqlite3_bind_text(st: *mut sqlite3_stmt, i: c_int, p: *const c_char, n: c_int, d: isize) -> c_int;
    fn sqlite3_bind_blob(st: *mut sqlite3_stmt, i: c_int, p: *const c_void, n: c_int, d: isize) -> c_int;
    fn sqlite3_bind_null(st: *mut sqlite3_stmt, i: c_int) -> c_int;
    fn sqlite3_bind_parameter_count(st: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_column_count(st: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_column_int64(st: *mut sqlite3_stmt, i: c_int) -> i64;
    fn sqlite3_column_blob(st: *mut sqlite3_stmt, i: c_int) -> *const c_void;
    fn sqlite3_column_text(st: *mut sqlite3_stmt, i: c_int) -> *const u8;
    fn sqlite3_column_bytes(st: *mut sqlite3_stmt, i: c_int) -> c_int;
    fn sqlite3_column_type(st: *mut sqlite3_stmt, i: c_int) -> c_int;
    fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;
    fn sqlite3_changes64(db: *mut sqlite3) -> i64;
    fn sqlite3_last_insert_rowid(db: *mut sqlite3) -> i64;
    fn sqlite3_get_autocommit(db: *mut sqlite3) -> c_int;
    fn sqlite3_busy_timeout(db: *mut sqlite3, ms: c_int) -> c_int;
    fn sqlite3_db_config(db: *mut sqlite3, op: c_int, ...) -> c_int;
    fn sqlite3_progress_handler(db: *mut sqlite3, n: c_int, cb: Option<extern "C" fn(*mut c_void) -> c_int>, arg: *mut c_void);
    fn sqlite3_extended_result_codes(db: *mut sqlite3, on: c_int) -> c_int;
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_CONSTRAINT: c_int = 19;
const SQLITE_INTERRUPT: c_int = 9;
const SQLITE_NULL: c_int = 5;
const OPEN_READWRITE: c_int = 0x2;
const OPEN_CREATE: c_int = 0x4;
const OPEN_READONLY: c_int = 0x1;
const OPEN_NOMUTEX: c_int = 0x8000;
const PREPARE_PERSISTENT: u32 = 0x1;
const DBCONFIG_DEFENSIVE: c_int = 1010;
const DBCONFIG_TRUSTED_SCHEMA: c_int = 1017;
const DBCONFIG_ENABLE_LOAD_EXTENSION: c_int = 1005;
/// Query plans do not depend on bound values: without it, a bound LIMIT
/// makes SQLite parse and plan the statement again on every binding
/// (measured: half the time of a post page with comments).
const DBCONFIG_ENABLE_QPSG: c_int = 1007;
const TRANSIENT: isize = -1;

/// A value to bind.
#[derive(Clone, Copy, Debug)]
pub enum Val<'a> {
    Int(i64),
    Text(&'a [u8]),
    Blob(&'a [u8]),
    Null,
}

#[derive(Debug, PartialEq)]
pub enum DbErr {
    /// A constraint failed (unique, check, foreign key).
    Conflict,
    /// The query ran past its deadline.
    Timeout,
    /// Anything else (with SQLite's message).
    Other(String),
}

/// A row, during one step.
pub struct Row {
    st: *mut sqlite3_stmt,
    n: c_int,
}

impl Row {
    pub fn int(&self, i: usize) -> i64 {
        assert!((i as c_int) < self.n);
        // SAFETY: a valid statement positioned on a row; i < column count.
        unsafe { sqlite3_column_int64(self.st, i as c_int) }
    }

    /// The bytes of a text or blob column (empty for NULL): valid only
    /// during this step, which the borrow ties it to.
    pub fn bytes(&self, i: usize) -> &[u8] {
        assert!((i as c_int) < self.n);
        // SAFETY: as above; SQLite keeps the pointer valid until the next
        // step, reset or column call on this column; the slice borrows self.
        unsafe {
            if sqlite3_column_type(self.st, i as c_int) == SQLITE_NULL {
                return &[];
            }
            let p = sqlite3_column_blob(self.st, i as c_int) as *const u8;
            let n = sqlite3_column_bytes(self.st, i as c_int);
            if p.is_null() || n <= 0 { &[] } else { std::slice::from_raw_parts(p, n as usize) }
        }
    }

    pub fn text(&self, i: usize) -> &str {
        assert!((i as c_int) < self.n);
        // SAFETY: as above; SQLite text columns are UTF-8 (we store only
        // UTF-8: every text we bind is a &str or checked bytes).
        let b = unsafe {
            let p = sqlite3_column_text(self.st, i as c_int);
            let n = sqlite3_column_bytes(self.st, i as c_int);
            if p.is_null() || n <= 0 { &[][..] } else { std::slice::from_raw_parts(p, n as usize) }
        };
        std::str::from_utf8(b).unwrap_or("")
    }

    pub fn is_null(&self, i: usize) -> bool {
        // SAFETY: as above.
        unsafe { sqlite3_column_type(self.st, i as c_int) == SQLITE_NULL }
    }
}

pub struct Db {
    raw: *mut sqlite3,
    stmts: Vec<*mut sqlite3_stmt>,
    /// When the running query must stop (monotonic ms; 0: never). Boxed so
    /// its address, given to the progress handler, never moves.
    deadline: Box<u64>,
}

// A Db is used by one thread at a time (SQLITE_THREADSAFE=2); moving it to
// another thread (a helper's reader) is fine.
unsafe impl Send for Db {}

/// Deadlines on (the default). The simulator turns them off: they read
/// the real clock, so whether one fired would differ from run to run.
static DEADLINES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn deadlines(on: bool) {
    DEADLINES.store(on, std::sync::atomic::Ordering::Relaxed);
}

extern "C" fn progress(arg: *mut c_void) -> c_int {
    // SAFETY: arg is the Db's boxed deadline, alive as long as the Db.
    let deadline = unsafe { *(arg as *const u64) };
    (deadline != 0 && DEADLINES.load(std::sync::atomic::Ordering::Relaxed) && crate::sys::linux::now_ms() >= deadline) as c_int
}

fn cstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    assert!(!v.contains(&0));
    v.push(0);
    v
}

impl Db {
    /// Opens (creating if write) the database at path, in WAL mode, with
    /// defensive settings.
    pub fn open(path: &str, write: bool) -> Result<Db, DbErr> {
        let p = cstr(path);
        let mut raw = std::ptr::null_mut();
        let flags = if write { OPEN_READWRITE | OPEN_CREATE } else { OPEN_READONLY } | OPEN_NOMUTEX;
        // SAFETY: p is NUL-terminated; raw receives the handle.
        let rc = unsafe { sqlite3_open_v2(p.as_ptr() as *const c_char, &mut raw, flags, std::ptr::null()) };
        let mut db = Db { raw, stmts: vec![], deadline: Box::new(0) };
        if rc != SQLITE_OK {
            return Err(db.err(rc));
        }
        // SAFETY: a valid handle; the deadline box lives as long as db.
        unsafe {
            sqlite3_extended_result_codes(raw, 0);
            sqlite3_busy_timeout(raw, 5000);
            sqlite3_db_config(raw, DBCONFIG_DEFENSIVE, 1 as c_int, std::ptr::null_mut::<c_int>());
            sqlite3_db_config(raw, DBCONFIG_TRUSTED_SCHEMA, 0 as c_int, std::ptr::null_mut::<c_int>());
            sqlite3_db_config(raw, DBCONFIG_ENABLE_LOAD_EXTENSION, 0 as c_int, std::ptr::null_mut::<c_int>());
            sqlite3_db_config(raw, DBCONFIG_ENABLE_QPSG, 1 as c_int, std::ptr::null_mut::<c_int>());
            let arg = &*db.deadline as *const u64 as *mut c_void;
            sqlite3_progress_handler(raw, 1000, Some(progress), arg);
        }
        let init = if write {
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON; PRAGMA cell_size_check = ON; PRAGMA cache_size = -65536;"
        } else {
            "PRAGMA query_only = ON; PRAGMA foreign_keys = ON; PRAGMA cell_size_check = ON; PRAGMA cache_size = -16384;"
        };
        db.exec(init)?;
        Ok(db)
    }

    fn err(&self, rc: c_int) -> DbErr {
        if rc & 0xff == SQLITE_CONSTRAINT {
            return DbErr::Conflict;
        }
        if rc & 0xff == SQLITE_INTERRUPT {
            return DbErr::Timeout;
        }
        // SAFETY: errmsg returns a NUL-terminated string owned by SQLite.
        let m = unsafe {
            let p = sqlite3_errmsg(self.raw);
            if p.is_null() { String::new() } else { std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned() }
        };
        DbErr::Other(format!("{rc}: {m}"))
    }

    /// Runs SQL text with no parameters (schema, pragmas, transactions).
    pub fn exec(&mut self, sql: &str) -> Result<(), DbErr> {
        let s = cstr(sql);
        // SAFETY: s is NUL-terminated; no callback.
        let rc = unsafe { sqlite3_exec(self.raw, s.as_ptr() as *const c_char, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut()) };
        if rc == SQLITE_OK { Ok(()) } else { Err(self.err(rc)) }
    }

    /// Prepares a statement for the life of the connection: its index.
    pub fn prepare(&mut self, sql: &str) -> Result<usize, DbErr> {
        let s = cstr(sql);
        let mut st = std::ptr::null_mut();
        // SAFETY: s is NUL-terminated; st receives the statement.
        let rc = unsafe { sqlite3_prepare_v3(self.raw, s.as_ptr() as *const c_char, -1, PREPARE_PERSISTENT, &mut st, std::ptr::null_mut()) };
        if rc != SQLITE_OK || st.is_null() {
            return Err(self.err(rc));
        }
        self.stmts.push(st);
        Ok(self.stmts.len() - 1)
    }

    /// Runs statement id with args; f sees each row. Answers the rows seen,
    /// or an error (the statement is reset either way).
    pub fn query(&mut self, id: usize, args: &[Val], mut f: impl FnMut(&Row)) -> Result<usize, DbErr> {
        let st = self.stmts[id];
        // SAFETY: st is a statement of this connection; values are copied
        // by SQLite (TRANSIENT) during the bind.
        unsafe {
            let want = sqlite3_bind_parameter_count(st);
            assert_eq!(want as usize, args.len(), "statement {id}: {} parameters, {} given", want, args.len());
            for (i, a) in args.iter().enumerate() {
                let k = i as c_int + 1;
                let rc = match *a {
                    Val::Int(v) => sqlite3_bind_int64(st, k, v),
                    Val::Text(t) => sqlite3_bind_text(st, k, t.as_ptr() as *const c_char, t.len() as c_int, TRANSIENT),
                    Val::Blob(b) => sqlite3_bind_blob(st, k, b.as_ptr() as *const c_void, b.len() as c_int, TRANSIENT),
                    Val::Null => sqlite3_bind_null(st, k),
                };
                if rc != SQLITE_OK {
                    sqlite3_reset(st);
                    sqlite3_clear_bindings(st);
                    return Err(self.err(rc));
                }
            }
            let row = Row { st, n: sqlite3_column_count(st) };
            let mut n = 0;
            let rc = loop {
                let rc = sqlite3_step(st);
                if rc != SQLITE_ROW {
                    break rc;
                }
                f(&row);
                n += 1;
            };
            sqlite3_reset(st);
            sqlite3_clear_bindings(st);
            if rc == SQLITE_DONE { Ok(n) } else { Err(self.err(rc)) }
        }
    }

    /// Runs a statement that returns no rows: the rows it changed.
    pub fn run(&mut self, id: usize, args: &[Val]) -> Result<u64, DbErr> {
        self.query(id, args, |_| {})?;
        // SAFETY: a valid handle.
        Ok(unsafe { sqlite3_changes64(self.raw) } as u64)
    }

    /// How long a statement waits for another connection's write lock
    /// (5 s by default). The simulator sets 0: its connections take turns
    /// in one thread, so a wait would only sleep; busy is an answer there.
    pub fn busy_wait(&mut self, ms: u32) {
        // SAFETY: a valid handle.
        unsafe { sqlite3_busy_timeout(self.raw, ms.min(i32::MAX as u32) as c_int) };
    }

    /// A transaction is open on this connection.
    pub fn in_transaction(&self) -> bool {
        // SAFETY: a valid handle.
        unsafe { sqlite3_get_autocommit(self.raw) == 0 }
    }

    pub fn last_rowid(&self) -> i64 {
        // SAFETY: a valid handle.
        unsafe { sqlite3_last_insert_rowid(self.raw) }
    }

    /// The first row's first column as an integer, if any.
    pub fn one_int(&mut self, id: usize, args: &[Val]) -> Result<Option<i64>, DbErr> {
        let mut v = None;
        self.query(id, args, |r| {
            if v.is_none() {
                v = Some(r.int(0));
            }
        })?;
        Ok(v)
    }

    /// Queries from now stop after ms (0: no limit).
    /// How many times each statement was prepared again by SQLite (a
    /// schema change, or a plan that depends on bound values): for
    /// profiling (tools and tests), not for the server's logic.
    pub fn reprepares(&self) -> Vec<(usize, i32)> {
        const SQLITE_STMTSTATUS_REPREPARE: c_int = 5;
        self.stmts
            .iter()
            .enumerate()
            // SAFETY: each statement is valid until drop; status reads a counter.
            .map(|(i, &st)| (i, unsafe { sqlite3_stmt_status(st, SQLITE_STMTSTATUS_REPREPARE, 0) }))
            .filter(|&(_, n)| n > 0)
            .collect()
    }

    pub fn set_deadline(&mut self, ms: u64) {
        *self.deadline = if ms == 0 { 0 } else { crate::sys::linux::now_ms() + ms };
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        // SAFETY: each statement and the handle are finalized exactly once.
        unsafe {
            for st in self.stmts.drain(..) {
                sqlite3_finalize(st);
            }
            sqlite3_close_v2(self.raw);
        }
    }
}
