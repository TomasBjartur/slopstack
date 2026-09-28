// THE LAW, as the oracle runs it: an executable reading of spec/authz.rs
// `permitted`, proved equal to it by Verus (tools/check.sh), and written
// apart from src/authz.rs (which the server uses), so that a bug there,
// in a build without proofs (the simulation's own mutation tests), cannot
// hide from the oracle.
use crate::spec_authz::*;
use vstd::prelude::*;

verus! {

pub fn permits(f: &Facts, a: Action) -> (r: bool)
    ensures r == permitted(*f, a),
{
    let user = f.who != 0;
    let owner = match f.role { Some(Role::Owner) => true, _ => false };
    let member = f.role.is_some();
    // The loaded post, if any: (id, blog, published).
    let (pid, pblog, public) = match f.post { Some(x) => (x.id, x.blog, x.published), None => (0, 0, false) };
    let has_post = f.post.is_some();
    match a {
        Action::CreateBlog => user,
        Action::EditBlog { blog } | Action::DeleteBlog { blog } => user && blog != 0 && f.blog == blog && owner,
        Action::AddAuthor { blog, user: u } => user && blog != 0 && f.blog == blog && owner && u != 0,
        Action::RemoveAuthor { blog, user: u } => user && blog != 0 && f.blog == blog && owner && u != f.who,
        Action::CreatePost { blog } | Action::ReadBlogAdmin { blog } => user && blog != 0 && f.blog == blog && member,
        Action::EditPost { post } | Action::PublishPost { post } | Action::DeletePost { post } =>
            user && f.blog != 0 && member && has_post && pid == post && post != 0 && pblog == f.blog,
        Action::ReadPost { post } =>
            (has_post && pid == post && post != 0 && public)
            || (user && f.blog != 0 && member && has_post && pid == post && post != 0 && pblog == f.blog),
        Action::Comment { post } | Action::Like { post } => user && has_post && pid == post && post != 0 && public,
        Action::DeleteComment { post, author } =>
            user && (author == f.who || (f.blog != 0 && member && has_post && pid == post && post != 0 && pblog == f.blog)),
    }
}

} // verus!
