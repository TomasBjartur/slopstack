#!/bin/sh
# The server, without proofs (tools/check.sh runs them): build/server.
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
VERUS="$HOME/opt/verus/verus-x86-linux/verus"
mkdir -p build
tools/build_c.sh >/dev/null
tools/build_wasm.sh
"$VERUS" src/main.rs --no-verify --compile -C linker="$HOME/opt/rustlink/cc.sh" -L build -l static=sqlite3 -l static=hacl -C opt-level=3 -o build/server 2>&1 | grep -v "unwindlib\|linker_messages\|^  |$\|^  = note\|^warning: 1 warning\|^warning: linker stderr\|^$" || true
test -x build/server
