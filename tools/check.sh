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
echo "== proofs (Lean, about the CRDT's model)"
( cd lean && "$HOME/.elan/bin/lean" Fugue.lean )
echo "== build"
"$VERUS" src/main.rs --no-verify --compile $LINK -C opt-level=3 -o build/server
echo "== tests"
# (Each test's own exit status decides: a pipe into tail would hide it.)
for t in crdt crypto db http markdown sim; do
  echo "-- $t"
  build/server test $t > build/test-$t.log 2>&1 || { tail -20 build/test-$t.log; echo "FAILED: $t (build/test-$t.log)"; exit 1; }
  tail -4 build/test-$t.log
done
echo "== the whole application, simulated (src/tests/appsim: a fixed seed budget, then a replay)"
build/server test appsim > build/test-appsim.log 2>&1 || { grep -A3 "VIOLATION\|FAIL" build/test-appsim.log | head -40; echo "FAILED: the simulation (build/test-appsim.log; each failing seed prints how to replay it)"; exit 1; }
tail -2 build/test-appsim.log
echo "== the browser's WebAssembly against the server's build"
tools/build_wasm.sh
"$HOME/opt/node/bin/node" tests/wasm_test.mjs | tail -1
echo "== the CRDT against the Lean model"
tests/crdt_lean.sh 50 | tail -1
