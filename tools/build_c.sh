#!/bin/sh
# The vendored C libraries (vendor/README.md), checked, then built once
# into static libraries in build/.
set -eu
cd "$(dirname "$0")/.."
CC="$HOME/opt/LLVM-19.1.7-Linux-X64/bin/clang"
AR="$HOME/opt/LLVM-19.1.7-Linux-X64/bin/llvm-ar"
mkdir -p build
echo "b1dd5d74ec7f29055a6684fa06fb3c2f6821c87dd38f9a458dfd2e8a1db28189  vendor/sqlite/sqlite3.c" | sha256sum -c --quiet
got=$(cd vendor/hacl && sha256sum $(find . -type f | sort) | sha256sum | cut -c1-64)
[ "$got" = "0aa86fa28d52ffbd9422be4a603293f2bc5a0aaf312d3d7d08d913bbee9752bf" ] || { echo "vendor/hacl changed: $got"; exit 1; }
if [ ! -f build/libsqlite3.a ] || [ vendor/sqlite/sqlite3.c -nt build/libsqlite3.a ]; then
  # Threads: each connection is used by one thread at a time (MULTITHREAD).
  $CC -O2 -fPIC -DSQLITE_DQS=0 -DSQLITE_THREADSAFE=2 -DSQLITE_DEFAULT_MEMSTATUS=0 \
    -DSQLITE_DEFAULT_WAL_SYNCHRONOUS=1 -DSQLITE_DEFAULT_FOREIGN_KEYS=1 -DSQLITE_LIKE_DOESNT_MATCH_BLOBS -DSQLITE_MAX_EXPR_DEPTH=0 \
    -DSQLITE_OMIT_DEPRECATED -DSQLITE_OMIT_LOAD_EXTENSION -DSQLITE_OMIT_SHARED_CACHE \
    -DSQLITE_USE_ALLOCA -DSQLITE_ENABLE_FTS5 -c vendor/sqlite/sqlite3.c -o build/sqlite3.o
  $AR rcs build/libsqlite3.a build/sqlite3.o
fi
if [ ! -f build/libhacl.a ] || [ -n "$(find vendor/hacl -newer build/libhacl.a)" ]; then
  for f in Hacl_P256 Hacl_Hash_SHA2; do
    $CC -O2 -fPIC -Ivendor/hacl -Ivendor/hacl/krml -c vendor/hacl/$f.c -o build/$f.o
  done
  $AR rcs build/libhacl.a build/Hacl_P256.o build/Hacl_Hash_SHA2.o
fi
