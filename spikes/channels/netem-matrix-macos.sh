#!/usr/bin/env bash
# Run the channel spike on loopback under emulated network conditions (macOS dummynet).
# Must run as root (pf/dnctl); the spike binaries run as the invoking user.
# Everything this script changes in pf/dummynet is undone on exit.
#
# Usage: sudo spikes/channels/netem-matrix-macos.sh [seconds-per-run]
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }
USER_NAME="${SUDO_USER:?run via sudo from your user account}"

HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="$HERE/target/release/channels"
[[ -x "$BIN" ]] || { echo "build first: cargo build --release --manifest-path $HERE/Cargo.toml" >&2; exit 1; }

SECS="${1:-8}"
PORT=47000
ANCHOR="com.apple/portalswitch-netem"
OUT="$HERE/results/netem-$(date +%Y%m%d-%H%M%S).jsonl"
TOKEN=""

as_user() { sudo -u "$USER_NAME" "$@"; }

cleanup() {
  pkill -f "$BIN server" 2>/dev/null || true
  pfctl -a "$ANCHOR" -F all 2>/dev/null || true
  dnctl -q pipe 1 delete 2>/dev/null || true
  dnctl -q pipe 2 delete 2>/dev/null || true
  [[ -n "$TOKEN" ]] && pfctl -X "$TOKEN" 2>/dev/null || true
  [[ -f "$OUT" ]] && chown "$USER_NAME" "$OUT" || true
  echo "netem removed"
}
trap cleanup EXIT INT TERM

# Stock ruleset contains `dummynet-anchor "com.apple/*"`; make sure it is loaded.
pfctl -q -f /etc/pf.conf
TOKEN="$(pfctl -E 2>&1 | awk '/Token/ { print $3 }')"

# netem <name> <bandwidth Mbit/s> <one-way delay ms> <packet loss ratio>
netem() {
  dnctl -q pipe 1 config bw "$2Mbit/s" delay "$3" plr "$4"
  dnctl -q pipe 2 config bw "$2Mbit/s" delay "$3" plr "$4"
  printf '%s\n' \
    "dummynet in quick on lo0 proto { tcp, udp } from any to any port $PORT pipe 1" \
    "dummynet in quick on lo0 proto { tcp, udp } from any port $PORT to any pipe 2" \
    | pfctl -q -a "$ANCHOR" -f -
}

mkdir -p "$HERE/results" && chown "$USER_NAME" "$HERE/results"
as_user "$BIN" server --bind "127.0.0.1:$PORT" 2>/dev/null &
sleep 1

SCENARIOS=(
  "lan-1g        1000 0.25 0"
  "lan-1g-loss1  1000 0.25 0.01"
  "wifi-200m      200 2    0.005"
  "wifi-bad-50m    50 5    0.02"
)

for s in "${SCENARIOS[@]}"; do
  read -r name bw delay loss <<<"$s"
  netem "$name" "$bw" "$delay" "$loss"
  for t in tcp1 tcp2 quic; do
    for b in none saturate; do
      as_user "$BIN" client --connect "127.0.0.1:$PORT" --transport "$t" --bulk "$b" \
        --secs "$SECS" --mss 1448 --label "$name" | tee -a "$OUT"
    done
  done
done
echo "results: $OUT"
