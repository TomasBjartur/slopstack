#!/bin/sh
# Every check (CLAUDE.md): proofs, then the tests (property, 10x perf).
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$HOME/.elan/bin:$PATH"
VERUS="$HOME/opt/verus/verus-x86-linux/verus"
LINK="-C linker=$HOME/opt/rustlink/cc.sh"
mkdir -p build
echo "== proofs (Verus)"
"$VERUS" src/main.rs
echo "== build"
"$VERUS" src/main.rs --no-verify --compile $LINK -C opt-level=3 -o build/server
"$VERUS" tests/http_test.rs --no-verify --compile $LINK -C opt-level=3 -o build/http_test
echo "== tests"
build/http_test
