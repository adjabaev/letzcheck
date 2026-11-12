#!/bin/sh
# Builds the engine as it is at a git revision (default HEAD) with its own data file, from the
# downloads already in data/, and prints "engine.wasm@lod.txt" for test/regress.mjs.
# Kept in target/base/<commit>/, so each revision is built once.
set -e
REF="${1:-HEAD}"
SHA=$(git rev-parse --short "$REF")
DIR="target/base/$SHA"
if [ ! -d "$DIR" ]; then
  git worktree prune
  git worktree add -q --detach "$DIR" "$SHA" >&2
fi
if [ ! -f "$DIR/dist/letz.wasm" ] || [ ! -f "$DIR/data/lod.txt" ]; then
  mkdir -p "$DIR/data" && cp -r data/lod data/spell "$DIR/data/"
  (cd "$DIR" && make data/lod.txt wasm) >&2
fi
echo "$DIR/dist/letz.wasm@$DIR/data/lod.txt"
