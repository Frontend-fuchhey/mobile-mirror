#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

SDK_ROOT="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}"
if [[ ! -d "$SDK_ROOT" ]]; then
    echo "Error: Android SDK not found at $SDK_ROOT" >&2
    exit 1
fi

PLATFORM_DIR="$(find "$SDK_ROOT/platforms" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
BUILD_TOOLS_DIR="$(find "$SDK_ROOT/build-tools" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"

ANDROID_JAR="$PLATFORM_DIR/android.jar"
D8_BIN="$BUILD_TOOLS_DIR/d8"

echo "[Build] Using Android Platform : $(basename "$PLATFORM_DIR")"
echo "[Build] Using Build-Tools      : $(basename "$BUILD_TOOLS_DIR")"

rm -rf build
mkdir -p build/classes

mapfile -t JAVA_FILES < <(find src/main/java -name "*.java")
javac --release 11 -cp "$ANDROID_JAR" -d build/classes "${JAVA_FILES[@]}"

mapfile -t CLASS_FILES < <(find build/classes -name "*.class")
"$D8_BIN" --min-api 21 --lib "$ANDROID_JAR" --output server.jar "${CLASS_FILES[@]}"
rm -rf build

echo "[Build] Successfully built $SCRIPT_DIR/server.jar ($(wc -c < server.jar) bytes)"
