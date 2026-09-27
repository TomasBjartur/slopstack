// Page templates: functions writing HTML straight into a buffer. Two kinds
// of text only: the template's own, which must be a &'static str (source
// code, not data: nothing a user sends is 'static), and everything else,
// escaped by html::escape (proved: no < > " & left). Stored posts are
// rendered by markdown::render into Markup (proved allowed markup).
// data-* attributes are code to Datastar: only template text writes them,
// with server-written ids and slugs (escaped) inside. Every action a user
// may repeat carries k=Date.now(): Datastar 1.0.4 does not send a second
// request to a URL it has sent one to from the same element (found by
// tests/social_test.py; the server ignores k).
use crate::html::{escape, Markup};

pub struct H {
    pub b: Vec<u8>,
    pub nonce: String,
    /// The page runs WebAssembly (the editor): its CSP allows compiling it.
    pub wasm: bool,
}

impl H {
    pub fn new(nonce: String) -> H {
        H { b: Vec::with_capacity(16 * 1024), nonce, wasm: false }
    }
    /// Template text.
    pub fn r(&mut self, s: &'static str) -> &mut H {
        self.b.extend_from_slice(s.as_bytes());
        self
    }
    /// Text from anywhere else, escaped.
    pub fn t(&mut self, s: &str) -> &mut H {
        self.b.extend_from_slice(&escape(s.as_bytes()));
        self
    }
    pub fn n(&mut self, v: u64) -> &mut H {
        let mut d = [0u8; 20];
        self.b.extend_from_slice(crate::app::itoa(v, &mut d));
        self
    }
    /// Allowed markup (a rendered post).
    pub fn m(&mut self, m: &Markup) -> &mut H {
        self.b.extend_from_slice(m.bytes());
        self
    }
    fn nonce(&mut self) -> &mut H {
        let n = std::mem::take(&mut self.nonce);
        self.b.extend_from_slice(n.as_bytes());
        self.nonce = n;
        self
    }
}

/// Asset URLs carry the assets' version (cached forever).
pub fn asset(h: &mut H, name: &'static str) {
    h.r("/s/").r(name).r("?v=").r(crate::assets::version());
}

// LAYOUT
pub fn head(h: &mut H, title: &str) {
    h.r("<!doctype html><html lang=\"en\" data-nonce=\"").nonce()
        .r("\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"color-scheme\" content=\"light dark\"><title>")
        .t(title).r(" · slopstack</title><link rel=\"stylesheet\" href=\"");
    asset(h, "app.css");
    h.r("\"><script type=\"module\" src=\"");
    asset(h, "datastar.js");
    h.r("\"></script><script type=\"module\" src=\"");
    asset(h, "app.js");
    // Links are fetched ahead on hover (the next page is ready before the click).
    h.r("\"></script><script type=\"speculationrules\" nonce=\"").nonce()
        .r("\">{\"prefetch\":[{\"where\":{\"and\":[{\"href_matches\":\"/*\"},{\"not\":{\"href_matches\":\"/verify*\"}},{\"not\":{\"href_matches\":\"/edit/*\"}}]},\"eagerness\":\"moderate\"}]}</script></head><body><a class=\"skip\" href=\"#main\">Skip to content</a>");
}

pub fn top(h: &mut H, signed_in: bool) {
    h.r("<header class=\"top\"><div class=\"wrap wide\"><a class=\"brand\" href=\"/\">slop<span>stack</span></a><nav>");
    if signed_in {
        h.r("<a href=\"/\" class=\"hide-sm\">Home</a><a href=\"/dash\">Your blogs</a><form method=\"post\" action=\"/logout\"><button class=\"link\">Log out</button></form><form method=\"post\" action=\"/write\"><button class=\"primary\">Write</button></form>");
    } else {
        h.r("<a href=\"/login\">Log in</a><a class=\"btn primary\" href=\"/signup\">Start writing</a>");
    }
    h.r("</nav></div></header>");
}

pub fn open(h: &mut H, title: &str, signed_in: bool, wide: bool) {
    head(h, title);
    top(h, signed_in);
    h.r(if wide { "<main id=\"main\"><div class=\"wrap wide\">" } else { "<main id=\"main\"><div class=\"wrap\">" });
}

pub fn close(h: &mut H) {
    h.r("</div></main></body></html>\n");
}

pub fn error(h: &mut H, code: u16, signed_in: bool) {
    let (title, text) = match code {
        400 => ("That did not work", "Something in the request was not right. Go back and try again."),
        403 => ("Not yours", "You do not have access to this. Are you logged in with the right account?"),
        404 => ("Not found", "There is nothing here. The address may be mistyped, or the page was removed."),
        409 => ("Already taken", "That name or address is already in use. Try another."),
        429 => ("Slow down", "That was a lot of changes in a minute. Wait a moment and try again."),
        _ => ("Something went wrong", "The server could not do this just now. Try again in a moment."),
    };
    open(h, title, signed_in, false);
    h.r("<div class=\"center card\"><h1>").r(title).r("</h1><p class=\"muted\">").r(text).r("</p><p><a class=\"btn\" href=\"/\">Go home</a></p></div>");
    close(h);
}

// DATES AND COUNTS
/// "Sep 27, 2026" (UTC).
pub fn date(h: &mut H, ms: i64) {
    let days = ms.div_euclid(86_400_000);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    h.r(M[(m - 1) as usize]).r(" ").n(d as u64).r(", ").n(y as u64);
}

pub fn minutes(h: &mut H, words: i64) {
    let m = ((words.max(0) + 229) / 230).max(1) as u64;
    h.n(m).r(" min read");
}

fn meta(h: &mut H, handle: &str, author: &str, published_ms: i64, words: i64) {
    h.r("<div class=\"meta\"><a href=\"/u/").t(handle).r("\">").t(author).r("</a><span class=\"dot\"></span>");
    date(h, published_ms);
    h.r("<span class=\"dot\"></span>");
    minutes(h, words);
    h.r("</div>");
}

// FEEDS
pub struct FeedItem {
    pub blog_slug: String,
    pub blog_title: String,
    pub slug: String,
    pub title: String,
    pub published_ms: i64,
    pub author: String,
    pub handle: String,
    pub words: i64,
}

fn items(h: &mut H, rows: &[FeedItem], with_blog: bool) {
    h.r("<ul class=\"feed\" id=\"feed\">");
    for r in rows {
        h.r("<li>");
        if with_blog {
            h.r("<a class=\"pub\" href=\"/b/").t(&r.blog_slug).r("\">").t(&r.blog_title).r("</a>");
        }
        h.r("<h3><a href=\"/b/").t(&r.blog_slug).r("/").t(&r.slug).r("\">").t(&r.title).r("</a></h3>");
        meta(h, &r.handle, &r.author, r.published_ms, r.words);
        h.r("</li>");
    }
    h.r("</ul>");
}

fn pager(h: &mut H, base: &str, page: u64, more: bool) {
    h.r("<nav class=\"pager\" id=\"pager\">");
    if page > 1 {
        h.r("<a href=\"").t(base).r("?page=").n(page - 1).r("\">← Newer</a>");
    } else {
        h.r("<span></span>");
    }
    if more {
        h.r("<a href=\"").t(base).r("?page=").n(page + 1).r("\">Older posts →</a>");
    }
    h.r("</nav>");
}

// FEED PAGES: the main part (cached by the server: the same for every
// visitor until the database's feed generation changes) and the page
// around it (per visitor: the header, the masthead for the signed-out).
pub fn masthead(h: &mut H) {
    h.r("<section class=\"masthead\"><h1>A quiet place to write</h1><p class=\"lede\">Fast pages, no trackers, no passwords: you sign in with a passkey. Write alone or with co-authors, even offline.</p><a class=\"btn primary big\" href=\"/signup\">Start writing</a></section>");
}

pub fn home_main(h: &mut H, rows: &[FeedItem], page: u64, more: bool) {
    h.r("<div class=\"feed-head\"><h2>Recent posts</h2></div>");
    if rows.is_empty() {
        h.r("<div class=\"empty\"><p>Nothing published yet. Be the first!</p></div>");
    } else {
        items(h, rows, true);
    }
    pager(h, "/", page, more);
}

pub fn blog_main(h: &mut H, slug: &str, title: &str, owner: &str, owner_handle: &str, rows: &[FeedItem], page: u64, more: bool) {
    h.r("<header class=\"masthead\"><h1>").t(title).r("</h1><p class=\"muted\">by <a href=\"/u/").t(owner_handle).r("\">").t(owner).r("</a></p></header>");
    if rows.is_empty() {
        h.r("<div class=\"empty\"><p>No posts yet.</p></div>");
    } else {
        items(h, rows, false);
    }
    let base = format!("/b/{slug}");
    pager(h, &base, page, more);
}

pub fn author_main(h: &mut H, handle: &str, name: &str, rows: &[FeedItem], page: u64, more: bool) {
    h.r("<header class=\"masthead\"><h1>").t(name).r("</h1><p class=\"muted\">@").t(handle).r("</p></header>");
    if rows.is_empty() {
        h.r("<div class=\"empty\"><p>Nothing published yet.</p></div>");
    } else {
        items(h, rows, true);
    }
    let base = format!("/u/{handle}");
    pager(h, &base, page, more);
}

/// A feed page: the header for this visitor, then the (cached) main part.
pub fn feed_page(h: &mut H, title: &str, signed_in: bool, masthead_: bool, main: &[u8]) {
    open(h, title, signed_in, false);
    if masthead_ && !signed_in {
        masthead(h);
    }
    // (main was made by the functions above: template and escaped text.)
    h.b.extend_from_slice(main);
    close(h);
}

pub struct PostView<'a> {
    pub id: u64,
    pub blog_slug: &'a str,
    pub blog_title: &'a str,
    pub title: &'a str,
    pub author: &'a str,
    pub handle: &'a str,
    pub published_ms: i64,
    pub words: i64,
    pub published: bool,
    pub can_edit: bool,
    pub just_published: bool,
    /// A preview (the editor's text, as it would publish): said so, with
    /// the way back.
    pub preview: bool,
}

/// A published post's likes and comments: (likes, liked by the viewer,
/// comment count), the first threads, where more start, the last id.
pub struct Social<'a> {
    pub likes: i64,
    pub liked: bool,
    pub count: i64,
    pub comments: &'a [CommentView],
    pub more_after: Option<u64>,
    pub last: u64,
}

pub fn post(h: &mut H, signed_in: bool, v: &PostView, body: &Markup, social_: Option<&Social>) {
    open(h, v.title, signed_in, false);
    if v.just_published {
        h.r("<p class=\"notice\" role=\"status\">Your post is live. Share its address: readers see it now.</p>");
    }
    h.r("<article class=\"article\"><header><a class=\"pub\" href=\"/b/").t(v.blog_slug).r("\">").t(v.blog_title).r("</a><h1>").t(v.title).r("</h1><div class=\"byline\"><strong><a href=\"/u/").t(v.handle).r("\">").t(v.author).r("</a></strong><span class=\"dot\"></span>");
    date(h, v.published_ms);
    h.r("<span class=\"dot\"></span>");
    minutes(h, v.words);
    if v.preview {
        h.r("<span class=\"dot\"></span><span class=\"badge\">Preview: as it will look when published</span>");
    } else if !v.published {
        h.r("<span class=\"dot\"></span><span class=\"badge\">Draft: only authors can see this</span>");
    }
    if v.preview {
        h.r("<span class=\"actions\"><a class=\"btn\" href=\"/edit/").n(v.id).r("\">Back to editing</a></span>");
    } else if v.can_edit {
        h.r("<span class=\"actions\"><a class=\"btn\" href=\"/edit/").n(v.id).r("\">Edit</a></span>");
    }
    h.r("</div></header><div class=\"body\">").m(body).r("</div>");
    if let Some(sc) = social_ {
        social(h, signed_in, v.id, sc.likes, sc.liked, sc.count);
    }
    h.r("<footer><a href=\"/b/").t(v.blog_slug).r("\">← More from ").t(v.blog_title).r("</a><a href=\"/\">Recent posts</a></footer></article>");
    if let Some(sc) = social_ {
        comments(h, signed_in, v.id, sc.comments, sc.more_after, sc.last);
    }
    close(h);
}

// ACCOUNTS
pub fn signup(h: &mut H, note: &str, name: &str, handle: &str, email: &str) {
    open(h, "Start writing", false, false);
    h.r("<div class=\"center card\"><h1>Start writing</h1><p class=\"muted\">No password to remember: next, your device creates a passkey (fingerprint, face or PIN).</p>");
    if !note.is_empty() {
        h.r("<p class=\"notice\" role=\"alert\">").t(note).r("</p>");
    }
    h.r("<form method=\"post\" action=\"/signup\" class=\"stack\"><label>Your name <span class=\"hint\">Shown on your posts.</span><input name=\"name\" required maxlength=\"80\" autocomplete=\"name\" value=\"").t(name)
        .r("\"></label><label>Username <span class=\"hint\">Unique: 3 to 30 of a-z, 0-9 and _, starting with a letter.</span><input name=\"handle\" required minlength=\"3\" maxlength=\"31\" pattern=\"@?[A-Za-z][A-Za-z0-9_]{2,29}\" autocapitalize=\"none\" autocomplete=\"username\" placeholder=\"e.g. maya\" value=\"").t(handle)
        .r("\" data-bind:_h data-on:input__debounce.300ms=\"@get('/handle?h=' + encodeURIComponent($_h))\"><span class=\"form-note meta\" id=\"handle-note\" aria-live=\"polite\"></span></label><label>Email <span class=\"hint\">Only for getting back in if you lose your device.</span><input name=\"email\" type=\"email\" required maxlength=\"254\" autocomplete=\"email\" value=\"").t(email)
        .r("\"></label><button class=\"primary big\">Continue</button></form><p class=\"meta\">Already have an account? <a href=\"/login\">Log in</a></p></div>");
    close(h);
}

/// The username check as you type (a Datastar answer).
pub fn handle_note(h: &mut H, note: &'static str, ok: bool) {
    h.r(if ok { "<span class=\"form-note meta ok\" id=\"handle-note\" aria-live=\"polite\">" } else { "<span class=\"form-note meta bad\" id=\"handle-note\" aria-live=\"polite\">" }).r(note).r("</span>");
}

/// A page with one message.
pub fn message(h: &mut H, signed_in: bool, title: &'static str, text: &'static str) {
    open(h, title, signed_in, false);
    h.r("<div class=\"center card\"><h1>").r(title).r("</h1><p class=\"muted\">").r(text).r("</p></div>");
    close(h);
}

pub fn check_email(h: &mut H) {
    message(h, false, "Check your email", "If that address can be used, a link is on its way. It works once, for 30 minutes. Your account is only as safe as that inbox: whoever can read it can add a passkey.");
}

pub fn verify(h: &mut H, token_hex: &str, email: &str, adding: bool) {
    open(h, "Make your passkey", false, false);
    h.r("<div class=\"center card stack\"><h1>").r(if adding { "Add a new passkey" } else { "One last step" }).r("</h1><p class=\"muted\">Create a passkey for <strong>").t(email).r("</strong>. Your device will ask for your fingerprint, face or PIN.</p><button id=\"passkey-register\" class=\"primary big\" data-token=\"").t(token_hex).r("\">Create a passkey</button><p id=\"passkey-status\" class=\"meta\" role=\"status\"></p></div><script type=\"module\" src=\"");
    asset(h, "passkey.js");
    h.r("\"></script>");
    close(h);
}

pub fn login(h: &mut H, signed_in: bool) {
    open(h, "Log in", signed_in, false);
    h.r("<div class=\"center card stack\"><h1>Welcome back</h1><button id=\"passkey-login\" class=\"primary big\">Log in with a passkey</button><p id=\"passkey-status\" class=\"meta\" role=\"status\"></p><details><summary>Lost your passkey?</summary><form method=\"post\" action=\"/recover\" class=\"stack\"><label>Email <input name=\"email\" type=\"email\" required maxlength=\"254\" autocomplete=\"email\"></label><div><button>Email me a link</button></div></form></details><p class=\"meta\">New here? <a href=\"/signup\">Create an account</a></p></div><script type=\"module\" src=\"");
    asset(h, "passkey.js");
    h.r("\"></script>");
    close(h);
}

// DASHBOARD
pub struct BlogCard {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub owner: bool,
    pub posts: i64,
}

fn new_post_button(h: &mut H, slug: &str, label: &'static str) {
    h.r("<form method=\"post\" action=\"/dash/").t(slug).r("/posts\"><button class=\"primary\">").r(label).r("</button></form>");
}

pub fn dash(h: &mut H, blogs: &[BlogCard]) {
    open(h, "Your blogs", true, true);
    if blogs.is_empty() {
        h.r("<div class=\"center card\"><h1>Welcome!</h1><p class=\"muted\">Name your blog, and you are ready to write. You can add co-authors later.</p><form method=\"post\" action=\"/blogs\" class=\"stack\"><label>Blog name <input name=\"title\" required maxlength=\"200\" placeholder=\"e.g. Field Notes\" autofocus></label><button class=\"primary big\">Create my blog</button></form></div>");
    } else {
        h.r("<div class=\"dash-head\"><h1>Your blogs</h1></div><div class=\"grid\">");
        for b in blogs {
            h.r("<div class=\"card blogcard\"><h3><a href=\"/dash/").t(&b.slug).r("\">").t(&b.title).r("</a></h3><div class=\"meta\">").r(if b.owner { "Owner" } else { "Author" }).r("<span class=\"dot\"></span>").n(b.posts as u64).r(if b.posts == 1 { " post" } else { " posts" }).r("</div><div class=\"row\">");
            new_post_button(h, &b.slug, "New post");
            h.r("<a class=\"btn\" href=\"/dash/").t(&b.slug).r("\">Posts and authors</a></div></div>");
        }
        h.r("</div><details class=\"more\"><summary>Start another blog</summary><div class=\"center\"><form method=\"post\" action=\"/blogs\" class=\"card stack\"><h2>Start a new blog</h2><label>Name <input name=\"title\" required maxlength=\"200\"></label><label>Address <span class=\"hint\">Optional: lowercase letters, digits and dashes.</span><input name=\"slug\" maxlength=\"64\" pattern=\"[a-z0-9\\-]*\"></label><div><button class=\"primary\">Create blog</button></div></form></div></details>");
    }
    close(h);
}

pub struct PostRow {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub published: bool,
    pub updated_ms: i64,
}

pub struct Author {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub owner: bool,
}

pub fn people(h: &mut H, slug: &str, is_owner: bool, authors: &[Author]) {
    h.r("<ul class=\"people\" id=\"people\">");
    for a in authors {
        h.r("<li><span><strong>").t(&a.name).r("</strong><br><span class=\"meta\">").r(if a.owner { "Owner" } else { "Author" }).r("<span class=\"dot\"></span>").t(&a.email).r("</span></span>");
        if is_owner && !a.owner {
            h.r("<form method=\"post\" action=\"/dash/").t(slug).r("/authors/").n(a.id as u64).r("/remove\" data-confirm=\"Remove ").t(&a.name).r(" from this blog? They will no longer be able to write or edit here.\" data-ok=\"Remove\" data-on:submit=\"@post('/dash/").t(slug).r("/authors/").n(a.id as u64).r("/remove?k=' + Date.now(), {contentType: 'form'})\"><button class=\"danger\">Remove</button></form>");
        }
        h.r("</li>");
    }
    h.r("</ul>");
}

pub fn dash_blog(h: &mut H, slug: &str, title: &str, is_owner: bool, posts: &[PostRow], authors: &[Author], note: &str) {
    open(h, title, true, true);
    h.r("<p class=\"meta crumb\"><a href=\"/dash\">← Your blogs</a></p><div class=\"dash-head\"><h1>").t(title).r("</h1><div class=\"row\"><a class=\"btn\" href=\"/b/").t(slug).r("\">View blog</a>");
    new_post_button(h, slug, "New post");
    h.r("</div></div><div class=\"cols\"><section>");
    if posts.is_empty() {
        h.r("<div class=\"empty\"><p>Nothing here yet.</p>");
        new_post_button(h, slug, "Write your first post");
        h.r("</div>");
    } else {
        h.r("<table class=\"table\">");
        for p in posts {
            h.r("<tr><td><a class=\"title\" href=\"/edit/").n(p.id as u64).r("\">").t(&p.title).r("</a><div class=\"meta\">edited ");
            date(h, p.updated_ms);
            h.r("</div></td><td>").r(if p.published { "<span class=\"badge live\">Published</span>" } else { "<span class=\"badge\">Draft</span>" }).r("</td><td>");
            if p.published {
                h.r("<a class=\"btn quiet view\" href=\"/b/").t(slug).r("/").t(&p.slug).r("\">View</a>");
            }
            h.r("<a class=\"btn\" href=\"/edit/").n(p.id as u64).r("\">Edit</a></td></tr>");
        }
        h.r("</table>");
    }
    h.r("</section><aside><h2>Authors</h2>");
    people(h, slug, is_owner, authors);
    if is_owner {
        h.r("<form id=\"add-author\" method=\"post\" action=\"/dash/").t(slug).r("/authors\" class=\"stack add-author\" data-on:submit=\"@post('/dash/").t(slug).r("/authors?k=' + Date.now(), {contentType: 'form'})\" data-indicator:_adding><label>Add a co-author <span class=\"hint\">They need an account first.</span><input name=\"email\" type=\"email\" required placeholder=\"their@email.com\"></label><p class=\"form-note meta\" id=\"add-author-note\" aria-live=\"polite\">").t(note).r("</p><div><button data-attr:disabled=\"$_adding\">Add author</button></div></form>");
        h.r("<form method=\"post\" action=\"/dash/").t(slug).r("/delete\" class=\"danger-zone\" data-confirm=\"Delete this blog and all its posts? This cannot be undone.\" data-ok=\"Delete\"><button class=\"danger\">Delete blog</button></form>");
    }
    h.r("</aside></div>");
    close(h);
}

/// The add-author note (a Datastar answer).
pub fn note(h: &mut H, note: &str) {
    h.r("<p class=\"form-note meta\" id=\"add-author-note\" aria-live=\"polite\">").t(note).r("</p>");
}

// EDITOR
pub struct EditView<'a> {
    pub id: u64,
    pub rep: u32,
    pub title: &'a str,
    pub slug: &'a str,
    pub published: bool,
    pub blog_slug: &'a str,
    pub body: &'a str,
    /// Too long to put in the page: the editor loads it.
    pub big: bool,
}

pub fn edit(h: &mut H, v: &EditView) {
    h.wasm = true;
    head(h, &format!("Editing: {}", v.title));
    h.r("<header class=\"editor-bar\"><div class=\"wrap wide\"><a class=\"btn quiet\" href=\"/dash/").t(v.blog_slug).r("\" aria-label=\"Back to the blog\">←</a><span id=\"sync-status\" class=\"status\" role=\"status\">Saved</span>");
    // Preview: the text as it will publish (for a published post, with the
    // changes not yet published), in a tab of its own (web/editor.js sends
    // unsent edits first).
    h.r("<a class=\"btn quiet\" id=\"preview\" href=\"/edit/").n(v.id).r("/preview\" target=\"preview\">Preview</a>");
    if v.published {
        h.r("<a class=\"btn quiet view\" href=\"/b/").t(v.blog_slug).r("/").t(v.slug).r("\">View</a><form method=\"post\" action=\"/edit/").n(v.id).r("/unpublish\" data-confirm=\"Take this post down? Readers will no longer see it.\" data-ok=\"Unpublish\"><button class=\"quiet\">Unpublish</button></form><button form=\"post-form\" name=\"action\" value=\"publish\" class=\"primary\">Update</button>");
    } else {
        h.r("<button form=\"post-form\" name=\"action\" value=\"save\">Save draft</button><button form=\"post-form\" name=\"action\" value=\"publish\" class=\"primary\">Publish</button>");
    }
    h.r("</div></header><main id=\"main\"><form id=\"post-form\" method=\"post\" action=\"/edit/").n(v.id).r("\" class=\"wrap write editor\"><textarea class=\"title\" name=\"title\" rows=\"1\" required maxlength=\"200\" placeholder=\"Title\" aria-label=\"Title\">&#10;").t(v.title).r("</textarea><div class=\"edit-tools\" id=\"edit-tools\"></div><textarea id=\"editor\" class=\"text\" ").r(if v.big { "readonly data-big=\"1\" " } else { "name=\"body\" " }).r("placeholder=\"Tell your story…\" aria-label=\"Text\" data-published=\"").r(if v.published { "1" } else { "0" }).r("\" data-post=\"").n(v.id).r("\" data-rep=\"").n(v.rep as u64).r("\" data-wasm=\"");
    asset(h, "app.wasm");
    h.r("\">&#10;").t(v.body).r("</textarea><p class=\"help\"><button type=\"button\" id=\"image-add\" class=\"link\">Add an image</button> (or paste or drop one). <input type=\"file\" id=\"image-pick\" accept=\"image/*\" hidden> <span class=\"md-help\">Markdown (CommonMark): <code>## Heading</code> <code>**bold**</code> <code>*italic*</code> <code>[link](https://…)</code> <code>- list</code> <code>&gt; quote</code>.</span> Your text syncs as you type; Ctrl+S or ⌘S saves.</p></form><div class=\"wrap danger-zone\"><form method=\"post\" action=\"/edit/").n(v.id).r("/delete\" data-confirm=\"Delete this post? This cannot be undone.\"><button class=\"danger\">Delete post</button></form></div></main><script type=\"module\" src=\"");
    asset(h, "editor.js");
    h.r("\"></script></body></html>\n");
}

// SOCIAL: likes and comments on a published post (Datastar: a like shows
// at once from local signals, then the server's bar says what is true;
// comments arrive live). data-* expressions hold only numbers the server
// wrote (ids, counts).
pub fn social(h: &mut H, signed_in: bool, pid: u64, likes: i64, liked: bool, comments: i64) {
    h.r("<div class=\"social\" id=\"social\">");
    if signed_in {
        h.r("<form method=\"post\" action=\"/like/").n(pid).r("\" data-signals=\"{_lk: ").r(if liked { "true" } else { "false" }).r(", _ln: ").n(likes.max(0) as u64)
            .r("}\" data-on:submit=\"$_liking || ($_lk = !$_lk, $_ln = $_ln + ($_lk ? 1 : -1), el.elements.on.value = $_lk ? '1' : '0', @post('/like/").n(pid)
            .r("?k=' + Date.now(), {contentType: 'form'}))\" data-indicator:_liking><input type=\"hidden\" name=\"on\" value=\"").r(if liked { "0" } else { "1" })
            .r(if liked { "\"><button class=\"like on\" aria-pressed=\"true\" title=\"Unlike\"" } else { "\"><button class=\"like\" aria-pressed=\"false\" title=\"Like\"" })
            .r(" data-class:on=\"$_lk\" data-attr:aria-pressed=\"$_lk ? 'true' : 'false'\" data-attr:title=\"$_lk ? 'Unlike' : 'Like'\">♥ <span data-text=\"$_ln\">").n(likes.max(0) as u64).r("</span></button></form>");
    } else {
        h.r("<a class=\"btn like\" href=\"/login\" title=\"Log in to like\">♥ ").n(likes.max(0) as u64).r("</a>");
    }
    h.r("<a class=\"btn quiet\" href=\"#comments\">").n(comments.max(0) as u64).r(if comments == 1 { " comment" } else { " comments" }).r("</a></div>");
}

pub struct CommentView {
    pub id: u64,
    pub parent: u64,
    pub author: String,
    pub handle: String,
    pub created_ms: i64,
    pub deleted: bool,
    /// The viewer may delete it (its author, or a moderator).
    pub can_delete: bool,
    pub body: std::rc::Rc<Markup>,
}

/// Replies nest to this depth on screen (deeper ones join the last level).
const NEST_MAX: usize = 4;

fn comment_open(h: &mut H, c: &CommentView, fresh: bool) {
    h.r(if fresh { "<div class=\"comment fresh\" id=\"c" } else { "<div class=\"comment\" id=\"c" }).n(c.id).r("\" data-parent=\"").n(c.parent).r("\"><div class=\"meta\">");
    if c.handle.is_empty() {
        h.t(&c.author);
    } else {
        h.r("<a href=\"/u/").t(&c.handle).r("\">").t(&c.author).r("</a>");
    }
    h.r("<span class=\"dot\"></span><a href=\"#c").n(c.id).r("\">");
    date(h, c.created_ms);
    h.r("</a></div><div class=\"cbody\" id=\"cb").n(c.id).r("\">");
    if c.deleted {
        h.r("<p class=\"muted\">[deleted]</p>");
    } else {
        h.m(&c.body);
    }
    h.r("</div>");
}

fn comment_actions(h: &mut H, c: &CommentView, signed_in: bool) {
    h.r("<div class=\"cactions\" id=\"ca").n(c.id).r("\">");
    if signed_in {
        h.r("<a href=\"/reply/").n(c.id).r("\" data-on:click__prevent=\"@get('/reply/").n(c.id).r("?k=' + Date.now())\">Reply</a>");
    }
    if c.can_delete && !c.deleted {
        h.r("<details class=\"del\"><summary>Delete</summary><form method=\"post\" action=\"/comment/").n(c.id).r("/delete\" data-on:submit=\"@post('/comment/").n(c.id)
            .r("/delete?k=' + Date.now(), {contentType: 'form'})\"><button class=\"danger\">Delete this comment</button></form></details>");
    }
    h.r("</div><div class=\"rf\" id=\"rf").n(c.id).r("\"></div>");
}

/// Comments in thread order, nested (cs: threads and their replies, in id
/// order; a reply's parent comes before it).
pub fn thread(h: &mut H, cs: &[CommentView], signed_in: bool) {
    use std::collections::HashMap;
    let mut kids: HashMap<u64, Vec<usize>> = HashMap::new();
    let ids: std::collections::HashSet<u64> = cs.iter().map(|c| c.id).collect();
    let mut roots = vec![];
    for (i, c) in cs.iter().enumerate() {
        if c.parent == 0 || !ids.contains(&c.parent) {
            roots.push(i);
        } else {
            kids.entry(c.parent).or_default().push(i);
        }
    }
    // Explicit stack (no recursion): Open(i, depth) or Close.
    enum F {
        Open(usize, usize),
        Close,
    }
    let mut stack: Vec<F> = roots.iter().rev().map(|&i| F::Open(i, 0)).collect();
    while let Some(f) = stack.pop() {
        match f {
            F::Close => {
                h.r("</details></div>");
            }
            F::Open(i, d) => {
                let c = &cs[i];
                comment_open(h, c, false);
                comment_actions(h, c, signed_in);
                let ks = kids.get(&c.id).map_or(&[][..], |v| &v[..]);
                if ks.is_empty() {
                    h.r("<details class=\"replies\" id=\"r").n(c.id).r("\" open><summary class=\"none\"></summary>");
                } else {
                    h.r("<details class=\"replies\" id=\"r").n(c.id).r("\" open><summary>").n(ks.len() as u64).r(if ks.len() == 1 { " reply" } else { " replies" }).r("</summary>");
                }
                stack.push(F::Close);
                for &k in ks.iter().rev() {
                    stack.push(F::Open(k, (d + 1).min(NEST_MAX)));
                }
            }
        }
    }
}

/// One comment arriving live (appended to its parent's replies).
pub fn comment_live(h: &mut H, c: &CommentView, signed_in: bool) {
    comment_open(h, c, true);
    comment_actions(h, c, signed_in);
    h.r("<details class=\"replies\" id=\"r").n(c.id).r("\" open><summary class=\"none\"></summary></details></div>");
}

/// The comment box (parent 0: a new thread).
pub fn comment_box(h: &mut H, pid: u64, parent: u64, id: &'static str, label: &'static str, cancel: bool) {
    h.r("<form id=\"").r(id);
    if parent != 0 {
        h.n(parent);
    }
    h.r("\" method=\"post\" action=\"/comment/").n(pid).r("\" class=\"stack cform\" data-on:submit=\"@post('/comment/").n(pid)
        .r("?k=' + Date.now(), {contentType: 'form'})\" data-indicator:_sending><input type=\"hidden\" name=\"after\" data-bind:cafter><input type=\"hidden\" name=\"parent\" value=\"").n(parent)
        .r("\"><textarea name=\"body\" required maxlength=\"10000\" rows=\"3\" placeholder=\"").r(label).r("…\" aria-label=\"").r(label)
        .r("\"></textarea><div class=\"row\"><button class=\"primary\" data-attr:disabled=\"$_sending\">Send</button>");
    if cancel {
        h.r("<button type=\"button\" class=\"quiet\" data-on:click=\"document.getElementById('rf").n(parent).r("').replaceChildren()\">Cancel</button>");
    }
    h.r("</div></form>");
}

/// The element that keeps comments coming: each answer replaces it with
/// the next request (after: the last comment shown; delay: wait first).
/// (The last comment shown is the signal cafter: live answers and sent
/// comments both bring everything after it, and move it.)
pub fn live(h: &mut H, pid: u64, n: u64) {
    h.r("<div id=\"live\" data-init__delay.10s=\"@get('/live/").n(pid).r("?n=").n(n)
        .r("&amp;after=' + $cafter, {requestCancellation: 'cleanup'})\"></div>");
}

pub fn more_comments(h: &mut H, pid: u64, after: u64) {
    h.r("<div id=\"more-comments\" class=\"more-comments\"><a class=\"btn\" href=\"/comments/").n(pid).r("?after=").n(after)
        .r("\" data-on:click__prevent=\"@get('/comments/").n(pid).r("?after=").n(after).r("')\" data-indicator:_morec data-attr:aria-busy=\"$_morec\">More comments</a></div>");
}

/// The comments section of a post page.
pub fn comments(h: &mut H, signed_in: bool, pid: u64, cs: &[CommentView], more_after: Option<u64>, last: u64) {
    h.r("<section class=\"comments\" id=\"comments\" data-signals:cafter=\"").n(last).r("\"><h2>Comments</h2>");
    if signed_in {
        comment_box(h, pid, 0, "cform", "Write a comment", false);
    } else {
        h.r("<p class=\"muted\"><a href=\"/login\">Log in</a> or <a href=\"/signup\">sign up</a> to comment.</p>");
    }
    live(h, pid, 0);
    h.r("<div id=\"thread\">");
    thread(h, cs, signed_in);
    if let Some(a) = more_after {
        more_comments(h, pid, a);
    }
    h.r("</div></section>");
}
