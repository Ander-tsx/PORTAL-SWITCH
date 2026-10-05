#!/usr/bin/env bash
# Guided session for the cursor-parking experiments. Each phase is announced by voice;
# the observer notes what the on-screen cursor / Dock / hot corners do.
#
# Usage: spikes/macos-input-latency/park-session.sh [seconds-per-phase]
set -euo pipefail

SECS="${1:-8}"
APP="$HOME/Applications/PortalSwitch Latency Spike.app"
VOICE="Eddy (Spanish (Mexico))"

speak() { say -v "$VOICE" "$1" 2>/dev/null || say "$1"; }

phase() {
  local n="$1" text="$2"; shift 2
  speak "Fase $n. $text"
  sleep 1
  open -n -W "$APP" --args park "$SECS" "$@"
  speak "Alto."
  sleep 2
}

phase 1 "Círculos." bg decouple
phase 2 "Círculos." hide swallow
phase 3 "Círculos." bg hide swallow
phase 4 "Empuja a la derecha, sin parar." bg hide swallow
phase 5 "Empuja a la esquina de abajo a la derecha." bg hide swallow
phase 6 "Desliza tres dedos hacia arriba, varias veces." bg hide swallow
phase 7 "Empuja a la esquina de abajo a la derecha." bg hide swallow edgewarp
speak "Terminado."
