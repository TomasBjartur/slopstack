// The SQLite binding and the schema: migrations apply once and are
// recorded; constraints fail as Conflict; bound values round-trip exactly
// (bytes, NUL, UTF-8); a runaway query stops at its deadline.
use crate::db;
use crate::sys::sqlite::{Db, DbErr, Val};

pub fn run() {
    let mut fails = 0;
    let mut check = |name: &str, ok: bool| {
        println!("{} {name}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            fails += 1;
        }
    };
    let dir = std::env::temp_dir().join(format!("web2-db-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.db");
    let p = path.to_str().unwrap();
    {
        let mut d = Db::open(p, true).unwrap();
        check("migrations apply", db::migrate(&mut d).is_ok());
        let v = d.prepare("PRAGMA user_version").unwrap();
        check("…and are recorded", d.one_int(v, &[]).unwrap() == Some(db::MIGRATIONS.len() as i64));
        check("…and apply only once", db::migrate(&mut d).is_ok());
        let ins = d.prepare("INSERT INTO user(email, name, handle, created_ms) VALUES (?1, ?2, 'u' || lower(hex(randomblob(4))), ?3)").unwrap();
        check("an insert", d.run(ins, &[Val::Text(b"a@example.com"), Val::Text(b"A"), Val::Int(1)]) == Ok(1));
        check("a unique email: a second is a conflict", d.run(ins, &[Val::Text(b"a@example.com"), Val::Text(b"B"), Val::Int(1)]) == Err(DbErr::Conflict));
        check("a check: an empty name is a conflict", d.run(ins, &[Val::Text(b"b@example.com"), Val::Text(b""), Val::Int(1)]) == Err(DbErr::Conflict));
        let h = d.prepare("INSERT INTO user(email, name, handle, created_ms) VALUES ('h@x.io', 'H', ?1, 1)").unwrap();
        for bad in [&b"Ann"[..], b"1ann", b"an", b"ann-b", b"ann\xc3\xa9", b""] {
            check(&format!("a bad handle {:?} is refused", String::from_utf8_lossy(bad)), d.run(h, &[Val::Text(bad)]) == Err(DbErr::Conflict));
        }
        let blog = d.prepare("INSERT INTO blog(slug, title, created_ms) VALUES (?1, ?2, 1)").unwrap();
        check("a slug with capitals is refused", d.run(blog, &[Val::Text(b"Bad"), Val::Text(b"T")]) == Err(DbErr::Conflict));
        let fk = d.prepare("INSERT INTO member(blog_id, user_id, role) VALUES (?1, ?2, 1)").unwrap();
        check("a foreign key to nothing is refused", d.run(fk, &[Val::Int(99), Val::Int(99)]) == Err(DbErr::Conflict));
        // Round trips.
        d.exec("CREATE TEMP TABLE t (b BLOB, s TEXT)").unwrap();
        let put = d.prepare("INSERT INTO t VALUES (?1, ?2)").unwrap();
        let get = d.prepare("SELECT b, s FROM t").unwrap();
        let blob = [0u8, 1, 255, 0, 7];
        let text = "caf\u{e9} \u{1f30a} \u{0}end";
        d.run(put, &[Val::Blob(&blob), Val::Text(text.as_bytes())]).unwrap();
        let mut back = (vec![], String::new());
        d.query(get, &[], |r| back = (r.bytes(0).to_vec(), r.text(1).to_string())).unwrap();
        check("bytes (with NULs) and UTF-8 round-trip exactly", back.0 == blob && back.1 == text);
        // A deadline stops a runaway query.
        let slow = d.prepare("WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c").unwrap();
        d.set_deadline(200);
        let t = std::time::Instant::now();
        let r = d.one_int(slow, &[]);
        d.set_deadline(0);
        check("a runaway query stops at its deadline", r == Err(DbErr::Timeout) && t.elapsed().as_millis() < 1000);
        check("…and the connection still works", d.run(put, &[Val::Null, Val::Text(b"after")]).is_ok());
    }
    let mut ro = Db::open(p, false).unwrap();
    let w = ro.prepare("INSERT INTO user(email, name, handle, created_ms) VALUES ('r@x', 'R', 'rrr', 1)");
    let refused = match w {
        Ok(id) => ro.run(id, &[]).is_err(),
        Err(_) => true,
    };
    check("a read-only connection cannot write", refused);
    let _ = std::fs::remove_dir_all(&dir);
    println!("\n{fails} failure(s)");
    if fails > 0 {
        std::process::exit(1);
    }
}
