#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The PortalSwitch Authors
# Real-network matrix for the channel spike. Run on the client machine; the server runs on the
# other machine (`channels server --bind 0.0.0.0:47000`).
#
# Usage: spikes/channels/lan-matrix.sh <server-ip> [replicas] [seconds-per-run] [label]
set -euo pipefail

HOST="${1:?server ip required}"
REPLICAS="${2:-3}"
SECS="${3:-10}"
LABEL="${4:-lan}"
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="$HERE/target/release/channels"
OUT="$HERE/results/$LABEL-$(date +%Y%m%d-%H%M%S).jsonl"
mkdir -p "$HERE/results"

run() {
  "$BIN" client --connect "$HOST:47000" --secs "$SECS" --label "$LABEL-r$r" "$@" >> "$OUT" \
    || echo "{\"error\":\"r$r $*\"}" >> "$OUT"
}

echo "results: $OUT"
for r in $(seq 1 "$REPLICAS"); do
  for t in tcp2 quic; do
    run --transport "$t" --bulk none
    for dir in saturate reverse; do
      run --transport "$t" --bulk "$dir" --bulk-rate 0
      run --transport "$t" --bulk "$dir" --bulk-rate adaptive --target-ms 4
      run --transport "$t" --bulk "$dir" --bulk-rate adaptive --target-ms 8
    done
  done
  echo "replica $r done"
done
python3 "$HERE/summarize.py" "$OUT"
