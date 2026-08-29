#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/dist/skills/chainsaw-lead"

rm -rf "$ROOT/dist"
mkdir -p "$OUT"

cp -R "$ROOT/skills/chainsaw-lead/." "$OUT/"

mkdir -p "$OUT/supervisor"
cp "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" "$ROOT/rust-toolchain.toml" "$OUT/supervisor/"
cp -R "$ROOT/src" "$OUT/supervisor/src"

echo "Assembled skill at $OUT (install with: npx skills add ./dist)"
