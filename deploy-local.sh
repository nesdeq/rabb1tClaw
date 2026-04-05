#!/usr/bin/env bash
set -euo pipefail

TARGET="aarch64-unknown-linux-gnu"
BIN="target/${TARGET}/release/rabb1tclaw"
REMOTE="qp@192.168.64.3"
REMOTE_DIR="/home/qp/rabb1tclaw"

echo "── Deploy to ${REMOTE} ──────────────"

echo "  stopping rabb1tclaw..."
ssh "$REMOTE" "killall rabb1tclaw 2>/dev/null && echo '  killed' || echo '  not running'"

echo "  copying binary..."
scp -q "$BIN" "${REMOTE}:${REMOTE_DIR}/rabb1tclaw"

echo ""
echo "Done."
