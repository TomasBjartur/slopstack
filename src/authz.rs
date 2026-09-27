// Authorization, proved (Verus, about this code) against spec/authz.rs:
// - authorize(f, a) gives a Permit exactly when permitted(f, a);
// - a Permit cannot be made any other way (private fields, and a type
//   invariant: every Permit that exists satisfies permitted(facts,
//   action)); writes take one (src/db.rs), so no write happens without
//   permission;
// - csrf(site) is the CSRF law;
// - sanity theorems about the policy (below).
use vstd::prelude::*;
use crate::spec_authz::*;

verus! {

impl Clone for Role {
    fn clone(&self) -> (r: Self) ensures r == *self {
        match self { Role::Owner => Role::Owner, Role::Author => Role::Author }
    }
}
impl Copy for Role {}
impl Clone for PostInfo {
    fn clone(&self) -> (r: Self) ensures r == *self { PostInfo { id: self.id, blog: self.blog, published: self.published } }
}
impl Copy for PostInfo {}
impl Clone for Facts {
    fn clone(&self) -> (r: Self) ensures r == *self { Facts { who: self.who, blog: self.blog, role: self.role, post: self.post } }
}
impl Copy for Facts {}
impl Clone for Action {
    fn clone(&self) -> (r: Self) ensures r == *self { *self }
}
impl Copy for Action {}

fn is_owner(r: Option<Role>) -> (b: bool)
    ensures b == (r == Some(Role::Owner)),
{
    match r { Some(Role::Owner) => true, _ => false }
}

fn post_is_exec(f: &Facts, p: u64) -> (b: bool)
    ensures b == post_is(*f, p),
{
    match f.post { Some(x) => x.id == p && p != 0 && x.blog == f.blog, None => false }
}

fn post_public_exec(f: &Facts, p: u64) -> (b: bool)
    ensures b == post_public(*f, p),
{
    match f.post { Some(x) => x.id == p && p != 0 && x.published, None => false }
}

fn owner_of_exec(f: &Facts, b: u64) -> (r: bool)
    ensures r == owner_of(*f, b),
{
    f.who != 0 && b != 0 && f.blog == b && is_owner(f.role)
}

fn member_of_exec(f: &Facts, b: u64) -> (r: bool)
    ensures r == member_of(*f, b),
{
    f.who != 0 && b != 0 && f.blog == b && f.role.is_some()
}

fn can_write_post_exec(f: &Facts, p: u64) -> (r: bool)
    ensures r == can_write_post(*f, p),
{
    member_of_exec(f, f.blog) && post_is_exec(f, p)
}

pub fn permitted_exec(f: &Facts, a: Action) -> (r: bool)
    ensures r == permitted(*f, a),
{
    match a {
        Action::CreateBlog => f.who != 0,
        Action::EditBlog { blog } => owner_of_exec(f, blog),
        Action::DeleteBlog { blog } => owner_of_exec(f, blog),
        Action::AddAuthor { blog, user } => owner_of_exec(f, blog) && user != 0,
        Action::RemoveAuthor { blog, user } => owner_of_exec(f, blog) && user != f.who,
        Action::CreatePost { blog } => member_of_exec(f, blog),
        Action::EditPost { post } => can_write_post_exec(f, post),
        Action::PublishPost { post } => can_write_post_exec(f, post),
        Action::DeletePost { post } => can_write_post_exec(f, post),
        Action::ReadPost { post } => post_public_exec(f, post) || can_write_post_exec(f, post),
        Action::ReadBlogAdmin { blog } => member_of_exec(f, blog),
    }
}

/// Permission to do one action, decided on these facts. Only authorize
/// makes one.
pub struct Permit {
    facts: Facts,
    action: Action,
}

impl Permit {
    #[verifier::type_invariant]
    spec fn inv(self) -> bool {
        permitted(self.facts, self.action)
    }

    pub closed spec fn spec_facts(self) -> Facts {
        self.facts
    }

    pub closed spec fn spec_action(self) -> Action {
        self.action
    }

    pub fn facts(&self) -> (r: Facts)
        ensures r == self.spec_facts(), permitted(r, self.spec_action()),
    {
        proof { use_type_invariant(self); }
        self.facts
    }

    pub fn action(&self) -> (r: Action)
        ensures r == self.spec_action(), permitted(self.spec_facts(), r),
    {
        proof { use_type_invariant(self); }
        self.action
    }
}

pub fn authorize(f: Facts, a: Action) -> (r: Option<Permit>)
    ensures
        r.is_some() == permitted(f, a),
        r matches Some(p) ==> p.spec_facts() == f && p.spec_action() == a,
{
    if permitted_exec(&f, a) { Some(Permit { facts: f, action: a }) } else { None }
}

/// The CSRF law on a request's Sec-Fetch-Site header.
pub fn csrf(site: Option<&[u8]>) -> (r: bool)
    ensures r == csrf_ok(match site { Some(s) => Some(s@), None => None }),
{
    match site {
        None => false,
        Some(s) => {
            let want: [u8; 11] = [115, 97, 109, 101, 45, 111, 114, 105, 103, 105, 110];
            let ghost g = same_origin();
            assert(want@ =~= g);
            if s.len() != 11 {
                assert(s@.len() != g.len());
                return false;
            }
            let mut i: usize = 0;
            while i < 11
                invariant s.len() == 11, i <= 11, want@ =~= g, g == same_origin(), site == Some(s),
                    forall|j: int| 0 <= j < i ==> s@[j] == want@[j],
                decreases 11 - i,
            {
                if s[i] != want[i] {
                    assert(s@[i as int] != g[i as int]);
                    assert(s@ != g);
                    return false;
                }
                i += 1;
            }
            assert(s@ =~= g);
            true
        }
    }
}

/// The write budget law: may who, having made spent writes this minute,
/// make another?
pub fn budget_ok(spent: u64) -> (r: bool)
    ensures r == within_budget(spent),
{
    spent < WRITES_PER_MINUTE
}

// SANITY THEOREMS about the policy.

/// Nobody signed in may do anything but read a published post.
pub proof fn anonymous_only_reads(f: Facts, a: Action)
    requires f.who == 0, permitted(f, a),
    ensures a matches Action::ReadPost { post } && post_public(f, post),
{
}

/// Whoever may change a post is a member of the blog that post belongs to
/// (one blog's members cannot touch another blog's posts).
pub proof fn writers_are_members_of_the_posts_blog(f: Facts, a: Action)
    requires
        permitted(f, a),
        a matches Action::EditPost { .. } || a matches Action::PublishPost { .. } || a matches Action::DeletePost { .. },
    ensures
        f.who != 0,
        f.role.is_some(),
        f.post matches Some(x) && x.blog == f.blog,
{
}

/// Whoever may manage a blog (authors, deletion) may also write in it.
pub proof fn owners_are_members(f: Facts, b: u64)
    requires owner_of(f, b),
    ensures member_of(f, b), permitted(f, Action::CreatePost { blog: b }),
{
}

} // verus!
