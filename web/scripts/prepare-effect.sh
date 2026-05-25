#!/usr/bin/env bash
# Bootstrap the vendored Effect source the effect-ts skill reads from.
# Clones effect-smol into <repo-root>/.repos/effect when missing.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
target="$repo_root/.repos/effect"

if [ -d "$target/.git" ] || [ -d "$target/packages" ]; then
  exit 0
fi

echo "prepare-effect: cloning effect-smol into $target"
git clone --depth 1 https://github.com/Effect-TS/effect-smol "$target"
