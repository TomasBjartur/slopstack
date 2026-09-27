// Static files, built into the binary (include_bytes!: never copied into
// a String, never parsed). Served at /s/<name>?v=<version>; the version is
// a hash of all of them, so a page always asks for the files it was made
// with, and browsers may keep them forever (Cache::Immutable).
use std::sync::OnceLock;

pub struct Asset {
    pub name: &'static str,
    pub ctype: &'static str,
    pub bytes: &'static [u8],
}

pub const ASSETS: &[Asset] = &[
    Asset { name: "app.css", ctype: "text/css; charset=utf-8", bytes: include_bytes!("../web/app.css") },
    Asset { name: "app.js", ctype: "text/javascript; charset=utf-8", bytes: include_bytes!("../web/app.js") },
    Asset { name: "passkey.js", ctype: "text/javascript; charset=utf-8", bytes: include_bytes!("../web/passkey.js") },
    Asset { name: "editor.js", ctype: "text/javascript; charset=utf-8", bytes: include_bytes!("../web/editor.js") },
    // vendor/datastar/README.md: version, hash, why.
    Asset { name: "datastar.js", ctype: "text/javascript; charset=utf-8", bytes: include_bytes!("../vendor/datastar/datastar.js") },
];

/// 12 hex digits of the SHA-256 of every asset.
pub fn version() -> &'static str {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        let mut all = Vec::new();
        for a in ASSETS {
            all.extend_from_slice(a.name.as_bytes());
            all.extend_from_slice(&crate::sys::crypto::sha256(a.bytes));
        }
        crate::resp::hex(&crate::sys::crypto::sha256(&all)[..6])
    })
}

pub fn find(name: &[u8]) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.name.as_bytes() == name)
}
