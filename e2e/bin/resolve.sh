#!/usr/bin/env bash
# resolve.sh — substitute node + upstream paths into a profile template.
# Usage: bin/resolve.sh <template.json> <out.json> [upstream-js]
set -uo pipefail
SRC="$1"; DST="$2"
NODE="$(command -v node)"
UP="${3:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/bin/fast-mcp.js}"
mkdir -p "$(dirname "$DST")"
sed -e "s#__NODE__#${NODE}#g" -e "s#__FAST__#${UP}#g" "$SRC" > "$DST"
echo "resolved -> $DST"
