#!/usr/bin/env bash
# Bootstrap the vendored Effect source the effect-ts skill reads from.
# Clones effect-smol into <repo-root>/.repos/effect when missing.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
target="$repo_root/.repos/effect"
effect_ref="${EFFECT_SMOL_REF:-bb54252bb1e4a4019229c6dc4817b965c5a627fb}"

if [ -d "$target/.git" ] || [ -d "$target/packages" ]; then
  exit 0
fi

echo "prepare-effect: cloning effect-smol into $target"
git init "$target"
git -C "$target" remote add origin https://github.com/Effect-TS/effect-smol
git -C "$target" fetch --depth 1 origin "$effect_ref"
git -C "$target" checkout --detach FETCH_HEAD
