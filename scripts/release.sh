#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SKILL="$ROOT/skills/chainsaw-lead"
OUT="$SKILL/supervisor"

rm -rf "$OUT"
mkdir -p "$OUT"
cp "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" "$ROOT/rust-toolchain.toml" "$OUT/"
rsync -a --exclude tests.rs --exclude 'test_*.rs' "$ROOT/src/" "$OUT/src/"
printf '%s\n' /target >"$OUT/.gitignore"

echo "Supervisor sources synced to $OUT (install with: npx skills add alexeyv/chainsaw)"
