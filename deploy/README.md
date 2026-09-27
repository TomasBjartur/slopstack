# Deploying slopstack

Target: the same server as the first version (Ubuntu 24.04, user
`claude`), with Caddy in front for TLS. The one-time root steps are the
first version's (`setcap` on Caddy, `loginctl enable-linger`, ports 80 and
443 open); nothing new.

## Install / upgrade

```sh
deploy/install.sh      # builds, backs up the database, installs to ~/slopstack, restarts
```

Data: `~/slopstack-data/blog.db` (WAL). Backups: ten kept in
`~/slopstack-data/backups/`, one taken before every install.

## Replacing the first version (once)

The first version runs as the user units `bent` and `caddy` with data in
`~/bent-data`. To switch (the old data is kept untouched):

```sh
python3 -c 'import sqlite3,os; s=sqlite3.connect(os.path.expanduser("~/bent-data/blog.db")); d=sqlite3.connect("/tmp/old.db"); s.backup(d)'
tools/import_old.py /tmp/old.db ~/slopstack-data/blog.db   # prints what is left behind
systemctl --user disable --now bent
deploy/install.sh
```

Left behind by the conversion (not in this version): comments, likes,
images, tags, schedules, custom domains. Users, passkeys, sessions, blogs,
authors and posts (published text and the editor's current text) carry
over. To go back: `systemctl --user disable --now slopstack`, restore the
first version's `caddy.service` and `Caddyfile` (`~/web/deploy`), and
`systemctl --user enable --now bent caddy`.

**Test mode:** `BLOG_SIGNUP_DIRECT=1` (the default in install.sh) skips the
email step. Emails are not checked and there is no account recovery. Set it
to 0 once a mail sender exists. Account security is only as strong as the
email inbox: whoever can read it can add a passkey.

## Operations

- Logs: `journalctl --user -u slopstack -u caddy -f`
- Mail: links for sign-up and recovery are in the `outbox` table; nothing
  sends them yet.
