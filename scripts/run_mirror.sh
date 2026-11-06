#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${PORT:-12345}"
BITRATE="${BITRATE:-12000000}"
FPS="${FPS:-60}"
MAX_SIZE="${MAX_SIZE:-0}"
RECORD_PATH="${RECORD_PATH:-recordings/recording.mp4}"
HW_BACKEND="${HW_BACKEND:-nvdec}"

if [[ ! -f "$ROOT_DIR/server/server.jar" ]]; then
    "$ROOT_DIR/server/build.sh"
fi

cargo build --release --manifest-path "$ROOT_DIR/client/Cargo.toml"

adb push "$ROOT_DIR/server/server.jar" /data/local/tmp/server.jar
adb forward "tcp:${PORT}" "tcp:${PORT}"

cleanup() {
    kill "${SERVER_PID:-}" 2>/dev/null || true
    adb forward --remove "tcp:${PORT}" 2>/dev/null || true
}
trap cleanup EXIT

adb shell "CLASSPATH=/data/local/tmp/server.jar app_process / com.mirror.Server port=${PORT} bitrate=${BITRATE} fps=${FPS} max_size=${MAX_SIZE} cbr=true" &
SERVER_PID=$!

sleep 0.6
"$ROOT_DIR/client/target/release/mobile-mirror-client" --addr "127.0.0.1:${PORT}" --record "$RECORD_PATH" --hw "$HW_BACKEND"
