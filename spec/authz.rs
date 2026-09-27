// LAWS: who may do what (owned by people; a change here is a SECURITY
// DECISION). The policy decides from Facts: what the server loaded about
// the request. Facts are not trusted because they are here: every write
// re-checks them against the database inside its transaction (src/db.rs).
use vstd::prelude::*;

verus! {

pub enum Role {
    Owner,
    Author,
}

/// A post as loaded: its id, its blog, whether it is public.
pub struct PostInfo {
    pub id: u64,
    pub blog: u64,
    pub published: bool,
}

/// who: the session's user (0: nobody signed in). blog: the blog the
/// request is about (0: none). role: who's role in that blog. post: the
/// post the request is about, if any.
pub struct Facts {
    pub who: u64,
    pub blog: u64,
    pub role: Option<Role>,
    pub post: Option<PostInfo>,
}

pub enum Action {
    CreateBlog,
    EditBlog { blog: u64 },
    DeleteBlog { blog: u64 },
    AddAuthor { blog: u64, user: u64 },
    RemoveAuthor { blog: u64, user: u64 },
    CreatePost { blog: u64 },
    EditPost { post: u64 },
    PublishPost { post: u64 },
    DeletePost { post: u64 },
    ReadPost { post: u64 },
    ReadBlogAdmin { blog: u64 },
    Comment { post: u64 },
    /// author: the comment's author, as loaded (re-read by the delete).
    DeleteComment { post: u64, author: u64 },
    Like { post: u64 },
}

pub open spec fn is_user(f: Facts) -> bool {
    f.who != 0
}

pub open spec fn owner_of(f: Facts, b: u64) -> bool {
    is_user(f) && b != 0 && f.blog == b && f.role == Some(Role::Owner)
}

pub open spec fn member_of(f: Facts, b: u64) -> bool {
    is_user(f) && b != 0 && f.blog == b && f.role.is_some()
}

/// The loaded post is post p, of the blog the request is about.
pub open spec fn post_is(f: Facts, p: u64) -> bool {
    match f.post {
        Some(x) => x.id == p && p != 0 && x.blog == f.blog,
        None => false,
    }
}

pub open spec fn post_public(f: Facts, p: u64) -> bool {
    match f.post {
        Some(x) => x.id == p && p != 0 && x.published,
        None => false,
    }
}

pub open spec fn can_write_post(f: Facts, p: u64) -> bool {
    member_of(f, f.blog) && post_is(f, p)
}

/// THE POLICY.
pub open spec fn permitted(f: Facts, a: Action) -> bool {
    match a {
        // Any signed-in user may create a blog (and becomes its owner).
        Action::CreateBlog => is_user(f),
        // Only the owner edits or deletes a blog, or manages its authors.
        Action::EditBlog { blog } => owner_of(f, blog),
        Action::DeleteBlog { blog } => owner_of(f, blog),
        Action::AddAuthor { blog, user } => owner_of(f, blog) && user != 0,
        // The owner cannot remove themself (a blog always has its owner).
        Action::RemoveAuthor { blog, user } => owner_of(f, blog) && user != f.who,
        // Members write the posts of their blog.
        Action::CreatePost { blog } => member_of(f, blog),
        Action::EditPost { post } => can_write_post(f, post),
        Action::PublishPost { post } => can_write_post(f, post),
        Action::DeletePost { post } => can_write_post(f, post),
        // Anyone reads a published post; members also read drafts.
        Action::ReadPost { post } => post_public(f, post) || can_write_post(f, post),
        Action::ReadBlogAdmin { blog } => member_of(f, blog),
        // SECURITY DECISION (2026-09-27): comments and likes. Any signed-in
        // user may comment on and like a published post (never a draft).
        Action::Comment { post } => is_user(f) && post_public(f, post),
        Action::Like { post } => is_user(f) && post_public(f, post),
        // A comment is deleted by its author, or by an author of the blog
        // (moderation).
        Action::DeleteComment { post, author } =>
            is_user(f) && (author == f.who || can_write_post(f, post)),
    }
}

/// THE WRITE BUDGET. SECURITY DECISION (2026-09-27, carried over): a user
/// makes at most WRITES_PER_MINUTE writes a minute (saves, posts, blogs,
/// authors; not typing, which syncs with its own limits). spent: the
/// user's writes in the current minute, as loaded; the write re-checks it
/// and counts itself inside its transaction.
pub const WRITES_PER_MINUTE: u64 = 30;

pub open spec fn within_budget(spent: u64) -> bool {
    spent < WRITES_PER_MINUTE
}

/// THE CSRF LAW: a request that changes state carries
/// Sec-Fetch-Site: same-origin. Browsers set it and pages cannot forge it,
/// so a cross-site form or fetch never changes state (the session cookie is
/// also SameSite=Strict: two independent defenses). A client that sends no
/// Sec-Fetch-Site cannot change state; every current browser sends it.
/// "same-origin"
pub open spec fn same_origin() -> Seq<u8> {
    seq![115u8, 97, 109, 101, 45, 111, 114, 105, 103, 105, 110]
}

pub open spec fn csrf_ok(site: Option<Seq<u8>>) -> bool {
    match site {
        Some(s) => s == same_origin(),
        None => false,
    }
}

// Sanity laws about the policy itself (a mistake in it is likely to break
// one): proved in src/authz.rs.
//   - nobody signed in may do anything but read a published post;
//   - a user of one blog can never change another blog's posts;
//   - whoever may manage authors or delete a blog may also write its posts.

} // verus!
