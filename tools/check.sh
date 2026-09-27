#!/bin/sh
# Every check (CLAUDE.md): proofs, then the tests (property, 10x perf).
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$HOME/.elan/bin:$PATH"
VERUS="$HOME/opt/verus/verus-x86-linux/verus"
LINK="-C linker=$HOME/opt/rustlink/cc.sh -L build -l static=sqlite3 -l static=hacl"
mkdir -p build
tools/build_c.sh
echo "== proofs (Verus)"
"$VERUS" src/main.rs
echo "== build"
"$VERUS" src/main.rs --no-verify --compile $LINK -C opt-level=3 -o build/server
echo "== tests"
for t in crypto db http markdown sim; do
  echo "-- $t"
  build/server test $t | tail -4
done
