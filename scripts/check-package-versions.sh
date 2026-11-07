#!/usr/bin/env bash
# Fails when a Basecamp package changed since BASE without a higher version in its
# metadata.json. A release only publishes a new version, so an unbumped change
# would never reach anyone's Basecamp.
#
# usage: scripts/check-package-versions.sh <base-ref>
set -euo pipefail
base="${1:?usage: $0 <base-ref>}"
failed=0

check() {
  local name="$1" meta="$2"
  shift 2
  if [[ -z "$(git diff --name-only "$base"...HEAD -- "$@")" ]]; then
    echo "$name: unchanged"
    return
  fi
  local old new
  new=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$meta")
  if ! old=$(git show "$base:$meta" 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])' 2>/dev/null); then
    echo "$name: new package at $new"
    return
  fi
  if [[ "$old" == "$new" || "$(printf '%s\n%s\n' "$old" "$new" | sort -V | tail -1)" != "$new" ]]; then
    echo "::error file=$meta::$name changed but its version is still $old: raise \"version\" in $meta, or the release will skip it"
    failed=1
  else
    echo "$name: $old -> $new"
  fi
}

# The module bundles the library, so library changes are module changes too.
# The binary's entry point and the tests never reach it.
check logos_mcp basecamp/metadata.json \
  basecamp ':(exclude)basecamp/app' \
  src Cargo.toml ':(exclude)src/main.rs' ':(exclude,glob)src/**/tests.rs'
check logos_mcp_app basecamp/app/metadata.json basecamp/app

exit "$failed"
