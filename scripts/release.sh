#!/usr/bin/env bash
# Assemble the shipped supervisor crate from the root crate, minus tests.
# Usage: scripts/release.sh [OUT]   (default: skills/chainsaw-lead/supervisor)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SKILL="$ROOT/skills/chainsaw-lead"
OUT=${1:-"$SKILL/supervisor"}

rm -rf "$OUT"
mkdir -p "$OUT"
cp "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" "$ROOT/rust-toolchain.toml" "$OUT/"
rsync -a --exclude tests.rs --exclude 'test_*.rs' "$ROOT/src/" "$OUT/src/"
printf '%s\n' /target >"$OUT/.gitignore"

echo "Supervisor sources synced to $OUT (install with: npx skills add alexeyv/chainsaw)"
