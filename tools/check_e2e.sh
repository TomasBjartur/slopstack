#!/bin/sh
# The end-to-end suites (after tools/check.sh): the server over HTTP, the
# editor in real Chrome, worker processes, the fuzzer. Each starts its own
# server on a fresh database; run them one at a time (they share ports).
set -u
cd "$(dirname "$0")/.."
NODE="$HOME/opt/node/bin/node"
fails=0
run() {
  printf '%-28s ' "$*"
  out=$("$@" 2>&1)
  if [ $? -eq 0 ]; then echo "ok  $(echo "$out" | grep -E 'failure|checks|PASS split|histories' | tail -1)"; else fails=$((fails + 1)); echo "FAILED"; echo "$out" | grep -E '^FAIL' | head -20; fi
}
run python3 tests/app_test.py
run python3 tests/passkey_test.py
run python3 tests/sync_test.py
run python3 tests/redteam_test.py
run python3 tests/social_test.py
run python3 tests/workers_test.py
run python3 tests/browser_test.py
run python3 tests/collab_test.py
run python3 tests/editor_test.py
run python3 tests/large_test.py
for t in usability_test ux_test; do
  [ -f tests/$t.py ] && run python3 tests/$t.py
done
run "$NODE" tests/vim_test.mjs
run "$NODE" tests/history_test.mjs
run "$NODE" tests/visual_split_test.mjs
run "$NODE" tests/wasm_test.mjs
# The fuzzer needs a running server.
rm -f /tmp/fuzz_e2e.db*
PORT=8090 BLOG_DB=/tmp/fuzz_e2e.db build/server >/dev/null 2>&1 &
pid=$!
sleep 1
run python3 tests/fuzz_server.py 8090 20000 1
kill $pid
echo "$fails suite(s) failed"
[ "$fails" -eq 0 ]
