// The database: schema (migrations, counted by PRAGMA user_version) and
// the statements, prepared once. Every write that needs permission takes
// an authz::Permit (only authz::authorize makes one) and re-checks the
// facts the permit was decided on inside its transaction (the facts could
// have changed since they were loaded).
//
// Documents are stored for novel length: a post's collaborative text is
// batches of CRDT operations (binary runs, src/doc/) and a snapshot of the
// merged state. Its published text is Markdown (post.body_md), rendered
// when read (no HTML is stored: one source of truth).
use crate::sys::sqlite::{Db, DbErr, Row, Val};

pub const MIGRATIONS: &[&str] = &[
    // v1
    "CREATE TABLE user (
       id INTEGER PRIMARY KEY,
       email TEXT NOT NULL UNIQUE CHECK (length(email) BETWEEN 3 AND 254),
       name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
       -- Public, unique: a-z first, then a-z 0-9 _ (3 to 30).
       handle TEXT NOT NULL UNIQUE CHECK (length(handle) BETWEEN 3 AND 30 AND handle GLOB '[a-z]*' AND handle NOT GLOB '*[^a-z0-9_]*'),
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
       handle TEXT NOT NULL,
       purpose INTEGER NOT NULL CHECK (purpose IN (1, 2)),
       expires_ms INTEGER NOT NULL,
       used INTEGER NOT NULL DEFAULT 0 CHECK (used IN (0, 1))
     ) STRICT, WITHOUT ROWID;
     CREATE INDEX email_token_handle ON email_token(handle);
     -- Mail to send (a relay reads it; nothing sends yet).
     CREATE TABLE outbox (
       id INTEGER PRIMARY KEY,
       to_email TEXT NOT NULL,
       subject TEXT NOT NULL,
       body TEXT NOT NULL,
       created_ms INTEGER NOT NULL,
       sent_ms INTEGER
     ) STRICT;
     CREATE INDEX outbox_to ON outbox(to_email, created_ms);
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
       -- The published text (Markdown), as of the last publish or update.
       body_md TEXT NOT NULL DEFAULT '',
       -- INTERIM (until the collaborative document, doc_ops): the draft.
       draft_md TEXT NOT NULL DEFAULT '',
       UNIQUE (blog_id, slug)
     ) STRICT;
     CREATE INDEX post_recent ON post(published, published_ms);
     CREATE INDEX post_blog ON post(blog_id, published, published_ms);
     CREATE INDEX post_author ON post(author_id, published, published_ms);
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

// STATEMENTS: every query, by name (prepared once, at start, in order).
macro_rules! queries {
    ($($name:ident => $sql:expr,)*) => {
        #[derive(Clone, Copy)]
        #[repr(usize)]
        pub enum Q { $($name,)* }
        const SQL: &[&str] = &[$($sql,)*];
    };
}

queries! {
    SessionUser => "SELECT s.user_id, u.name, u.handle FROM session s JOIN user u ON u.id = s.user_id WHERE s.token_hash = ?1 AND s.expires_ms > ?2",
    SessionNew => "INSERT INTO session(token_hash, user_id, created_ms, expires_ms) VALUES (?1, ?2, ?3, ?4)",
    SessionDel => "DELETE FROM session WHERE token_hash = ?1",
    ChallengeNew => "INSERT INTO challenge(hash, purpose, email_token, expires_ms) VALUES (?1, ?2, ?3, ?4)",
    ChallengeTake => "DELETE FROM challenge WHERE hash = ?1 AND purpose = ?2 AND expires_ms > ?3 AND email_token IS ?4",
    TokenNew => "INSERT INTO email_token(hash, email, name, handle, purpose, expires_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    TokenGet => "SELECT email, name, handle, purpose FROM email_token WHERE hash = ?1 AND used = 0 AND expires_ms > ?2",
    TokenUse => "UPDATE email_token SET used = 1 WHERE hash = ?1 AND used = 0 AND expires_ms > ?2",
    UserByEmail => "SELECT id FROM user WHERE email = ?1",
    UserInfo => "SELECT name, handle FROM user WHERE id = ?1",
    UserByHandle => "SELECT id, name FROM user WHERE handle = ?1",
    HandleTaken => "SELECT 1 FROM user WHERE handle = ?1 UNION ALL SELECT 1 FROM email_token WHERE handle = ?1 AND used = 0 AND expires_ms > ?2 AND email != ?3 LIMIT 1",
    UserNew => "INSERT INTO user(email, name, handle, created_ms) VALUES (?1, ?2, ?3, ?4)",
    CredNew => "INSERT INTO credential(id, user_id, public_key, sign_count, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
    CredGet => "SELECT user_id, public_key, sign_count FROM credential WHERE id = ?1",
    CredCount => "UPDATE credential SET sign_count = ?2 WHERE id = ?1 AND sign_count = ?3",
    MailCount => "SELECT count(*) FROM outbox WHERE to_email = ?1 AND created_ms > ?2",
    Outbox => "INSERT INTO outbox(to_email, subject, body, created_ms) VALUES (?1, ?2, ?3, ?4)",
    Role => "SELECT role FROM member WHERE blog_id = ?1 AND user_id = ?2",
    PostInfo => "SELECT blog_id, published FROM post WHERE id = ?1",
    BlogBySlug => "SELECT id, title FROM blog WHERE slug = ?1",
    BudgetGet => "SELECT window_ms, n FROM write_budget WHERE user_id = ?1",
    BudgetPut => "INSERT INTO write_budget(user_id, window_ms, n) VALUES (?1, ?2, ?3) ON CONFLICT(user_id) DO UPDATE SET window_ms = ?2, n = ?3",
    BlogNew => "INSERT INTO blog(slug, title, created_ms) VALUES (?1, ?2, ?3)",
    SlugBlogTaken => "SELECT 1 FROM blog WHERE slug = ?1",
    MemberAdd => "INSERT INTO member(blog_id, user_id, role) VALUES (?1, ?2, ?3)",
    MemberDel => "DELETE FROM member WHERE blog_id = ?1 AND user_id = ?2 AND role = 2",
    BlogDel => "DELETE FROM blog WHERE id = ?1",
    BlogSlug => "SELECT slug FROM blog WHERE id = ?1",
    MyBlogs => "SELECT b.id, b.slug, b.title, m.role, (SELECT count(*) FROM post p WHERE p.blog_id = b.id) FROM member m JOIN blog b ON b.id = m.blog_id WHERE m.user_id = ?1 ORDER BY b.title LIMIT 1000",
    Authors => "SELECT u.id, u.name, u.email, m.role FROM member m JOIN user u ON u.id = m.user_id WHERE m.blog_id = ?1 ORDER BY m.role, u.name LIMIT 1000",
    PostNew => "INSERT INTO post(blog_id, slug, title, author_id, updated_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
    SlugTaken => "SELECT 1 FROM post WHERE blog_id = ?1 AND slug = ?2",
    PostsAdmin => "SELECT id, slug, title, published, updated_ms FROM post WHERE blog_id = ?1 ORDER BY updated_ms DESC LIMIT ?2 OFFSET ?3",
    EditGet => "SELECT p.title, p.slug, p.published, b.slug, b.id, p.draft_md, p.published_ms IS NULL FROM post p JOIN blog b ON b.id = p.blog_id WHERE p.id = ?1",
    DraftSave => "UPDATE post SET title = ?2, draft_md = ?3, updated_ms = ?4 WHERE id = ?1",
    PostPublish => "UPDATE post SET title = ?2, draft_md = ?3, body_md = ?3, words = ?4, published = 1, published_ms = coalesce(published_ms, ?5), updated_ms = ?5, slug = ?6 WHERE id = ?1",
    PostUnpublish => "UPDATE post SET published = 0, updated_ms = ?2 WHERE id = ?1",
    PostDel => "DELETE FROM post WHERE id = ?1",
    BlogPosts => "SELECT p.slug, p.title, p.published_ms, b.slug, b.title, coalesce(u.name, ''), coalesce(u.id, 0), p.words, coalesce(u.handle, '') FROM post p JOIN blog b ON b.id = p.blog_id LEFT JOIN user u ON u.id = p.author_id WHERE p.blog_id = ?1 AND p.published = 1 ORDER BY p.published_ms DESC LIMIT ?2 OFFSET ?3",
    PostPage => "SELECT p.id, p.title, p.published_ms, coalesce(u.name, ''), coalesce(u.handle, ''), p.words, p.published, p.updated_ms FROM post p LEFT JOIN user u ON u.id = p.author_id WHERE p.blog_id = ?1 AND p.slug = ?2",
    PostBody => "SELECT body_md FROM post WHERE id = ?1",
    AuthorPosts => "SELECT p.slug, p.title, p.published_ms, b.slug, b.title, coalesce(u.name, ''), coalesce(u.id, 0), p.words, coalesce(u.handle, '') FROM post p JOIN blog b ON b.id = p.blog_id LEFT JOIN user u ON u.id = p.author_id WHERE p.author_id = ?1 AND p.published = 1 ORDER BY p.published_ms DESC LIMIT ?2 OFFSET ?3",
    Recent => "SELECT p.slug, p.title, p.published_ms, b.slug, b.title, coalesce(u.name, ''), coalesce(u.id, 0), p.words, coalesce(u.handle, '') FROM post p JOIN blog b ON b.id = p.blog_id LEFT JOIN user u ON u.id = p.author_id WHERE p.published = 1 ORDER BY p.published_ms DESC LIMIT ?1 OFFSET ?2",
    OpsSince => "SELECT seq, data FROM doc_ops WHERE post_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
    OpsAdd => "INSERT INTO doc_ops(post_id, data) VALUES (?1, ?2)",
    OpsCount => "SELECT coalesce(sum(length(data)), 0), count(*) FROM doc_ops WHERE post_id = ?1 AND seq > ?2",
    SnapGet => "SELECT upto, data FROM doc_snap WHERE post_id = ?1",
    SnapPut => "INSERT INTO doc_snap(post_id, upto, data, size) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(post_id) DO UPDATE SET upto = ?2, data = ?3, size = ?4",
    Sweep => "DELETE FROM challenge WHERE expires_ms < ?1",
    SweepTokens => "DELETE FROM email_token WHERE expires_ms < ?1",
    SweepSessions => "DELETE FROM session WHERE expires_ms < ?1",
    Begin => "BEGIN IMMEDIATE",
    Commit => "COMMIT",
    Rollback => "ROLLBACK",
}

/// The server's database: the connection and its statements.
pub struct Store {
    pub db: Db,
    /// The id of the first statement of SQL (migrate prepares its own).
    base: usize,
}

/// Why a write did not happen.
#[derive(Debug, PartialEq)]
pub enum No {
    /// The facts it was decided on changed (or were never true).
    Denied,
    /// A constraint (a slug taken, an author already there).
    Conflict,
    /// The write budget for this minute is spent.
    Budget,
    /// The database failed.
    Error,
}

impl From<DbErr> for No {
    fn from(e: DbErr) -> No {
        match e {
            DbErr::Conflict => No::Conflict,
            _ => No::Error,
        }
    }
}

use crate::authz::Permit;
use crate::spec_authz::{Facts, PostInfo, Role};

impl Store {
    pub fn open(path: &str) -> Result<Store, DbErr> {
        let mut db = Db::open(path, true)?;
        migrate(&mut db)?;
        let mut base = usize::MAX;
        for (i, sql) in SQL.iter().enumerate() {
            let id = db.prepare(sql)?;
            if i == 0 {
                base = id;
            }
            assert_eq!(id, base + i, "statement order");
        }
        Ok(Store { db, base })
    }

    pub fn q(&mut self, q: Q, args: &[Val], f: impl FnMut(&Row)) -> Result<usize, DbErr> {
        self.db.set_deadline(250);
        let r = self.db.query(self.base + q as usize, args, f);
        self.db.set_deadline(0);
        r
    }

    pub fn run(&mut self, q: Q, args: &[Val]) -> Result<u64, DbErr> {
        self.db.run(self.base + q as usize, args)
    }

    pub fn one(&mut self, q: Q, args: &[Val]) -> Result<Option<i64>, DbErr> {
        self.db.one_int(self.base + q as usize, args)
    }

    // FACTS
    /// What the request is about, as the database has it now.
    pub fn facts(&mut self, who: u64, blog: u64, post: u64) -> Result<Facts, DbErr> {
        let mut blog = blog;
        let mut info = None;
        if post != 0 {
            let mut pi = None;
            self.q(Q::PostInfo, &[Val::Int(post as i64)], |r| pi = Some((r.int(0) as u64, r.int(1) != 0)))?;
            if let Some((b, published)) = pi {
                if blog == 0 {
                    blog = b;
                }
                info = Some(PostInfo { id: post, blog: b, published });
            }
        }
        let mut role = None;
        if who != 0 && blog != 0 {
            self.q(Q::Role, &[Val::Int(blog as i64), Val::Int(who as i64)], |r| {
                role = match r.int(0) {
                    1 => Some(Role::Owner),
                    2 => Some(Role::Author),
                    _ => None,
                }
            })?;
        }
        Ok(Facts { who, blog, role, post: info })
    }

    fn same(a: &Facts, b: &Facts) -> bool {
        let role = |r: &Option<Role>| match r {
            None => 0,
            Some(Role::Owner) => 1,
            Some(Role::Author) => 2,
        };
        let post = |p: &Option<PostInfo>| p.as_ref().map(|x| (x.id, x.blog, x.published));
        a.who == b.who && a.blog == b.blog && role(&a.role) == role(&b.role) && post(&a.post) == post(&b.post)
    }

    /// Runs a write for a permit: in one transaction, the permit's facts are
    /// loaded again (they must be what it was decided on), the user's write
    /// budget is spent, then f. Any failure rolls it all back.
    pub fn write<T>(&mut self, p: &Permit, now_ms: u64, budgeted: bool, f: impl FnOnce(&mut Store, &Facts) -> Result<T, No>) -> Result<T, No> {
        let facts = p.facts();
        self.run(Q::Begin, &[])?;
        let r = (|| {
            let now = self.facts(facts.who, facts.blog, facts.post.as_ref().map_or(0, |x| x.id))?;
            if !Store::same(&now, &facts) {
                return Err(No::Denied);
            }
            if budgeted && !self.spend(facts.who, now_ms)? {
                return Err(No::Budget);
            }
            f(self, &facts)
        })();
        match r {
            Ok(v) => {
                self.run(Q::Commit, &[])?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.run(Q::Rollback, &[]);
                Err(e)
            }
        }
    }

    /// Spends one write of who's budget (spec/authz.rs WRITES_PER_MINUTE);
    /// false if this minute's is spent.
    fn spend(&mut self, who: u64, now_ms: u64) -> Result<bool, DbErr> {
        let mut cur = None;
        self.q(Q::BudgetGet, &[Val::Int(who as i64)], |r| cur = Some((r.int(0) as u64, r.int(1) as u64)))?;
        let (window, spent) = match cur {
            Some((w, n)) if now_ms < w + 60_000 => (w, n),
            _ => (now_ms, 0),
        };
        if !crate::authz::budget_ok(spent) {
            return Ok(false);
        }
        self.run(Q::BudgetPut, &[Val::Int(who as i64), Val::Int(window as i64), Val::Int(spent as i64 + 1)])?;
        Ok(true)
    }

    // SESSIONS: made only for a passkey check that passed.
    /// A new session for user (token: 32 random bytes, stored hashed).
    pub fn session_for(&mut self, _proof: &crate::webauthn::Passed, user: u64, token: &[u8; 32], now_ms: u64) -> Result<(), DbErr> {
        let h = crate::sys::crypto::sha256(token);
        self.run(Q::SessionNew, &[Val::Blob(&h), Val::Int(user as i64), Val::Int(now_ms as i64), Val::Int((now_ms + SESSION_MS) as i64)]).map(|_| ())
    }

    /// The signed-in user (id, name) for a session token, if valid.
    pub fn session(&mut self, token: &[u8; 32], now_ms: u64) -> Option<(u64, String)> {
        let h = crate::sys::crypto::sha256(token);
        let mut out = None;
        self.q(Q::SessionUser, &[Val::Blob(&h), Val::Int(now_ms as i64)], |r| out = Some((r.int(0) as u64, r.text(1).to_string()))).ok()?;
        out
    }

    pub fn end_session(&mut self, token: &[u8; 32]) {
        let h = crate::sys::crypto::sha256(token);
        let _ = self.run(Q::SessionDel, &[Val::Blob(&h)]);
    }
}

/// A session lasts 30 days.
pub const SESSION_MS: u64 = 30 * 24 * 3600 * 1000;
