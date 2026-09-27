#!/bin/sh
# The browser's WebAssembly (src/wasm.rs: the CRDT and the Markdown
# renderer) -> build/app.wasm. Needs vstd for wasm32, built once from the
# Verus sources at the pinned commit (CLAUDE.md) into ~/opt/verus-wasm.
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
V="$HOME/opt/verus/verus-x86-linux"
S="$HOME/opt/verus-src/source"
O="$HOME/opt/verus-wasm"
COMMIT=aef82eda71838deef4a8cd0260fda6c9504be9b9
T=wasm32-unknown-unknown
if [ ! -f "$O/libvstd.rlib" ]; then
  if [ ! -d "$S" ]; then
    mkdir -p "$HOME/opt/verus-src"
    git -C "$HOME/opt/verus-src" clone -q --filter=blob:none --no-checkout https://github.com/verus-lang/verus.git .
    git -C "$HOME/opt/verus-src" sparse-checkout set source/vstd source/builtin source/builtin_macros
    git -C "$HOME/opt/verus-src" checkout -q "$COMMIT"
  fi
  test "$(git -C "$HOME/opt/verus-src" rev-parse HEAD)" = "$COMMIT"
  mkdir -p "$O"
  rustc --edition 2018 --target $T --crate-type rlib --crate-name verus_builtin -O "$S/builtin/src/lib.rs" -o "$O/libverus_builtin.rlib"
  VSTD_KIND=IsVstd rustc --edition 2021 --target $T --crate-type rlib --crate-name vstd -O \
    --cfg 'feature="std"' --cfg 'feature="alloc"' \
    --extern verus_builtin="$O/libverus_builtin.rlib" \
    --extern verus_builtin_macros="$V/libverus_builtin_macros.so" \
    --extern verus_state_machines_macros="$V/libverus_state_machines_macros.so" \
    -L "$O" "$S/vstd/vstd.rs" -o "$O/libvstd.rlib" 2>&1 | grep -v "^warning" | grep -E "^error" -A5 || true
fi
mkdir -p build
VSTD_KIND=Imported rustc --edition 2021 --target $T --crate-type cdylib --crate-name app \
  -C opt-level=3 -C lto=fat -C panic=abort -C strip=symbols \
  --extern vstd="$O/libvstd.rlib" \
  --extern verus_builtin="$O/libverus_builtin.rlib" \
  --extern verus_builtin_macros="$V/libverus_builtin_macros.so" \
  --extern verus_state_machines_macros="$V/libverus_state_machines_macros.so" \
  -L "$O" -L "dependency=$V" src/wasm.rs -o build/app.wasm 2>&1 | grep -E "^(error|warning: unused)" -A6 || true
test -f build/app.wasm
