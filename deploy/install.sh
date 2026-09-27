#!/bin/sh
# Installs (or upgrades) slopstack for the current user: runs every check,
# builds, backs up the database, copies the release to ~/slopstack, keeps
# data in ~/slopstack-data, (re)starts the user units. One-time root steps
# are in deploy/README.md.
set -eu
cd "$(dirname "$0")/.."
HOST="${SLOP_HOST:-slopstack.tomasbjartur.com}"
# Test deployment without a mail sender: sign-up goes straight to the
# passkey (no email check, no recovery). Set to 0 once mail is sent.
DIRECT="${BLOG_SIGNUP_DIRECT:-1}"
# Worker processes: one per core, leaving room for Caddy.
WORKERS="${BLOG_WORKERS:-2}"
tools/build.sh
mkdir -p "$HOME/slopstack" "$HOME/slopstack-data" "$HOME/.config/systemd/user"
chmod 700 "$HOME/slopstack-data"
# Back up the database before the new server migrates it (the backup API
# gives a consistent copy of a live WAL database; cp does not). Ten kept.
if [ -f "$HOME/slopstack-data/blog.db" ]; then
  mkdir -p "$HOME/slopstack-data/backups"
  python3 -c 'import sqlite3, sys; s = sqlite3.connect(sys.argv[1]); d = sqlite3.connect(sys.argv[2]); s.backup(d); d.close()' \
    "$HOME/slopstack-data/blog.db" "$HOME/slopstack-data/backups/blog-$(date -u +%Y%m%dT%H%M%SZ).db"
  ls -1t "$HOME/slopstack-data/backups"/blog-*.db | tail -n +11 | xargs -r rm --
fi
install -m 755 build/server "$HOME/slopstack/server.new" && mv "$HOME/slopstack/server.new" "$HOME/slopstack/server"
install -m 644 deploy/Caddyfile "$HOME/slopstack/Caddyfile"
cat > "$HOME/slopstack/env" <<ENV
PORT=8080
BLOG_DB=$HOME/slopstack-data/blog.db
BLOG_ORIGIN=https://$HOST
BLOG_RP_ID=$HOST
SLOP_HOST=$HOST
BLOG_SIGNUP_DIRECT=$DIRECT
BLOG_WORKERS=$WORKERS
ENV
install -m 644 deploy/slopstack.service deploy/caddy.service "$HOME/.config/systemd/user/"
systemctl --user daemon-reload
systemctl --user enable slopstack caddy >/dev/null
systemctl --user restart slopstack caddy
sleep 2
systemctl --user --no-pager status slopstack caddy | grep -E "Active|Main PID"
