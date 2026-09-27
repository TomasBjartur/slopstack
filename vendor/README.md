# Vendored C libraries

The only code we run that we did not write. Each: version, source,
checksum, why, and what we use. Nothing else is vendored or fetched.

## sqlite (3.53.4)
- Amalgamation from https://sqlite.org/2026/sqlite-amalgamation-3530400.zip
  (`sqlite3.c`, `sqlite3.h` only).
- sha256(sqlite3.c) = b1dd5d74ec7f29055a6684fa06fb3c2f6821c87dd38f9a458dfd2e8a1db28189
- Why: the database (WAL, one writer, many readers).
- Used through hand-written bindings: src/sys/sqlite.rs.
- Checked by: its own test suite upstream; our bindings by the DB tests and
  the simulator.

## hacl (HACL*, formally verified C)
- https://github.com/hacl-star/hacl-star at commit
  504c2987452f87fe44bce9b9f12e19d6e051761f (2026-04-10), from
  `dist/gcc-compatible` and `dist/karamel`: `Hacl_P256.c`,
  `Hacl_Hash_SHA2.c` and the headers they include. `config.h` is ours (the
  configure step's output: 128-bit integers available).
- sha256 of the files (sorted, `sha256sum $(find . -type f | sort) |
  sha256sum`): see tools/check.sh (checked on every build).
- Why: SHA-256 (session tokens, passkey data) and ECDSA P-256 verification
  (passkeys). HACL* is proved in F*/Low* memory safe, functionally correct
  against its specification, and secret-independent (constant time).
- Used through src/sys/crypto.rs.

## datastar (1.0.4)
- `vendor/datastar/datastar.js`, as in vendor/datastar/README.md.
- sha256 = 727844adfc825ee651fb93c544a2a739986f9a21820a94524b35f0cac470cf91
- Why: server interaction on every page (fragments patched in, optimistic
  signals) without a framework: DESIGN.md, "Feeling like an SPA".
- Served with a per-response CSP nonce; data-* expressions hold only
  server-written ids and slugs.
