#!/bin/sh
# The Rust CRDT against the Lean model's own definitions (lean/Fugue.lean
# `text`, `applyAll`): random histories, the same text required.
# usage: tests/crdt_lean.sh [cases]
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.elan/bin:$PATH"
n=${1:-100}
tmp=$(mktemp -d)
cat lean/Fugue.lean lean/Run.lean > "$tmp/M.lean"
fails=0
for s in $(seq 1 "$n"); do
  build/server test crdt-lean "$s" > "$tmp/case"
  want=$(grep '^=' "$tmp/case")
  got=$(lean --run "$tmp/M.lean" < "$tmp/case" 2>/dev/null | grep "^=" || true)
  if [ "$want" != "$got" ]; then
    fails=$((fails + 1))
    echo "FAIL seed $s"
  fi
done
echo "$n histories: Rust and the Lean model agree on $((n - fails))"
rm -rf "$tmp"
[ "$fails" = 0 ]
