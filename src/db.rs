// The database: schema (migrations, counted by PRAGMA user_version) and
// the statements, prepared once. Every write that needs permission takes
// an authz::Permit (only authz::authorize makes one) and re-checks the
// facts the permit was decided on inside its transaction (the facts could
// have changed since they were loaded).
//
// Documents are stored for novel length: a post's collaborative text is
// batches of CRDT operations (binary runs, src/doc/) and a snapshot of the
// merged state; its rendered HTML is stored per block, so a save
// re-renders only blocks that changed and a page is the blocks' bytes.
use crate::sys::sqlite::{Db, DbErr, Row, Val};

pub const MIGRATIONS: &[&str] = &[
    // v1
    "CREATE TABLE user (
       id INTEGER PRIMARY KEY,
       email TEXT NOT NULL UNIQUE CHECK (length(email) BETWEEN 3 AND 254),
       name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
       created_ms INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE credential (
       id BLOB PRIMARY KEY CHECK (length(id) BETWEEN 16 AND 1023),
       user_id INTEGER NOT NULL REFERENCES user(id) ON DELETE CASCADE,
       public_key BLOB NOT NULL CHECK (length(public_key) = 64),
       sign_count INTEGER NOT NULL CHECK (sign_count >= 0),
       created_ms INTEGER NOT NULL
     ) STRICT, WITHOUT ROWID;
     CREATE INDEX credential_user ON credential(user_id);
     CREATE TABLE session (
       token_hash BLOB PRIMARY KEY CHECK (length(token_hash) = 32),
       user_id INTEGER NOT NULL REFERENCES user(id) ON DELETE CASCADE,
       created_ms INTEGER NOT NULL,
       expires_ms INTEGER NOT NULL
     ) STRICT, WITHOUT ROWID;
     CREATE INDEX session_user ON session(user_id);
     -- A passkey ceremony's challenge (register or log in), by its hash; used once.
     CREATE TABLE challenge (
       hash BLOB PRIMARY KEY CHECK (length(hash) = 32),
       purpose INTEGER NOT NULL CHECK (purpose IN (1, 2)),
       email_token BLOB,
       expires_ms INTEGER NOT NULL
     ) STRICT, WITHOUT ROWID;
     -- An emailed link (sign up, or add a passkey to an account), by its hash; used once.
     CREATE TABLE email_token (
       hash BLOB PRIMARY KEY CHECK (length(hash) = 32),
       email TEXT NOT NULL,
       name TEXT NOT NULL,
       purpose INTEGER NOT NULL CHECK (purpose IN (1, 2)),
       expires_ms INTEGER NOT NULL,
       used INTEGER NOT NULL DEFAULT 0 CHECK (used IN (0, 1))
     ) STRICT, WITHOUT ROWID;
     -- Mail to send (a relay reads it; nothing sends yet).
     CREATE TABLE outbox (
       id INTEGER PRIMARY KEY,
       to_email TEXT NOT NULL,
       subject TEXT NOT NULL,
       body TEXT NOT NULL,
       created_ms INTEGER NOT NULL,
       sent_ms INTEGER
     ) STRICT;
     CREATE TABLE blog (
       id INTEGER PRIMARY KEY,
       slug TEXT NOT NULL UNIQUE CHECK (length(slug) BETWEEN 1 AND 64 AND slug NOT GLOB '*[^a-z0-9-]*'),
       title TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
       created_ms INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE member (
       blog_id INTEGER NOT NULL REFERENCES blog(id) ON DELETE CASCADE,
       user_id INTEGER NOT NULL REFERENCES user(id) ON DELETE CASCADE,
       role INTEGER NOT NULL CHECK (role IN (1, 2)),
       PRIMARY KEY (blog_id, user_id)
     ) STRICT, WITHOUT ROWID;
     CREATE INDEX member_user ON member(user_id);
     CREATE TABLE post (
       id INTEGER PRIMARY KEY,
       blog_id INTEGER NOT NULL REFERENCES blog(id) ON DELETE CASCADE,
       slug TEXT NOT NULL CHECK (length(slug) BETWEEN 1 AND 80 AND slug NOT GLOB '*[^a-z0-9-]*'),
       title TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
       author_id INTEGER REFERENCES user(id) ON DELETE SET NULL,
       published INTEGER NOT NULL DEFAULT 0 CHECK (published IN (0, 1)),
       published_ms INTEGER,
       updated_ms INTEGER NOT NULL,
       words INTEGER NOT NULL DEFAULT 0,
       UNIQUE (blog_id, slug)
     ) STRICT;
     CREATE INDEX post_blog ON post(blog_id, published, published_ms);
     CREATE INDEX post_author ON post(author_id, published, published_ms);
     -- The rendered post: its blocks' HTML in order (allowed markup only:
     -- src/markdown.rs, spec/markup.rs), keyed by the block's hash.
     CREATE TABLE post_block (
       post_id INTEGER NOT NULL REFERENCES post(id) ON DELETE CASCADE,
       n INTEGER NOT NULL,
       hash BLOB NOT NULL,
       html BLOB NOT NULL,
       PRIMARY KEY (post_id, n)
     ) STRICT, WITHOUT ROWID;
     -- The collaborative text: batches of operations, and a snapshot.
     CREATE TABLE doc_ops (
       seq INTEGER PRIMARY KEY AUTOINCREMENT,
       post_id INTEGER NOT NULL REFERENCES post(id) ON DELETE CASCADE,
       data BLOB NOT NULL CHECK (length(data) BETWEEN 1 AND 16777216)
     ) STRICT;
     CREATE INDEX doc_ops_post ON doc_ops(post_id, seq);
     CREATE TABLE doc_snap (
       post_id INTEGER PRIMARY KEY REFERENCES post(id) ON DELETE CASCADE,
       upto INTEGER NOT NULL,
       data BLOB NOT NULL,
       size INTEGER NOT NULL
     ) STRICT;
     -- Writes per user per minute (spec/authz.rs WRITES_PER_MINUTE).
     CREATE TABLE write_budget (
       user_id INTEGER PRIMARY KEY REFERENCES user(id) ON DELETE CASCADE,
       window_ms INTEGER NOT NULL,
       n INTEGER NOT NULL CHECK (n >= 0)
     ) STRICT;",
];

/// Brings the schema up to date (each migration in its own transaction).
pub fn migrate(db: &mut Db) -> Result<(), DbErr> {
    let v = {
        let id = db.prepare("PRAGMA user_version")?;
        db.one_int(id, &[])?.unwrap_or(0) as usize
    };
    for (i, m) in MIGRATIONS.iter().enumerate().skip(v) {
        db.exec("BEGIN IMMEDIATE")?;
        let r = db.exec(m).and_then(|_| db.exec(&format!("PRAGMA user_version = {}", i + 1)));
        match r {
            Ok(()) => db.exec("COMMIT")?,
            Err(e) => {
                let _ = db.exec("ROLLBACK");
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Reads a whole row's texts, for tests and small admin queries.
pub fn texts(r: &Row, n: usize) -> Vec<String> {
    (0..n).map(|i| r.text(i).to_string()).collect()
}

pub fn int(v: i64) -> Val<'static> {
    Val::Int(v)
}
