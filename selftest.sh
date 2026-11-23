#!/bin/sh
# Run the in-app self-test against throwaway repos in /tmp, with `cat` standing in for the agent (no model runs).
set -eu
cd "$(dirname "$0")"
swift build
ROOT=$(mktemp -d /tmp/xctx-st.XXXX)
mk() { mkdir -p "$ROOT/code/$1/src" && printf '%s\n' "$2" > "$ROOT/code/$1/README.md" && touch "$ROOT/code/$1/src/$3" &&
       git -C "$ROOT/code/$1" init -q -b main && git -C "$ROOT/code/$1" add . &&
       git -C "$ROOT/code/$1" -c user.name=t -c user.email=t@t commit -q -m first; }
mk payments "Payments service" limit.ts
mk checkout-web "Checkout frontend" Checkout.tsx
XCTX_DATA_DIR="$ROOT/data" XCTX_INDEX_ROOTS="$ROOT/code" XCTX_AGENT_CMD=cat XCTX_SELFTEST="$ROOT/report.txt" \
  "$(swift build --show-bin-path)/xctx" >/dev/null 2>"$ROOT/stderr.txt" &
PID=$!
i=0; while [ ! -f "$ROOT/report.txt" ] && [ $i -lt 120 ]; do sleep 1; i=$((i+1)); done
kill $PID 2>/dev/null || true
cat "$ROOT/report.txt" 2>/dev/null || { echo "no report"; tail -20 "$ROOT/stderr.txt"; exit 1; }
! grep -q '^FAIL' "$ROOT/report.txt" && rm -rf "$ROOT"
