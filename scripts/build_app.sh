#!/usr/bin/env bash
set -e

# Ensure Homebrew and Cargo are in PATH
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"

# Move to repository root
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

APP_NAME="MicMirror"
TARGET_DIR="target/release"
APP_BUNDLE="${TARGET_DIR}/${APP_NAME}.app"
CONTENTS_DIR="${APP_BUNDLE}/Contents"
MACOS_DIR="${CONTENTS_DIR}/MacOS"
RESOURCES_DIR="${CONTENTS_DIR}/Resources"

# Terminate any previously running instance so the new version launches cleanly
killall "${APP_NAME}" 2>/dev/null || true

echo "=== Building ${APP_NAME} in release mode ==="
cargo build --release

BIN_SOURCE="${TARGET_DIR}/micmirror"
if [ ! -f "${BIN_SOURCE}" ]; then
    BIN_SOURCE="target/aarch64-apple-darwin/release/micmirror"
fi

echo "=== Packaging ${APP_NAME}.app bundle from ${BIN_SOURCE} ==="
rm -rf "${APP_BUNDLE}"
mkdir -p "${MACOS_DIR}"
mkdir -p "${RESOURCES_DIR}"

# Copy binary
cp "${BIN_SOURCE}" "${MACOS_DIR}/${APP_NAME}"
chmod +x "${MACOS_DIR}/${APP_NAME}"

# Copy Info.plist
cp "Info.plist" "${CONTENTS_DIR}/Info.plist"

# Ad-hoc code sign for macOS local microphone permission granting
echo "=== Ad-hoc signing ${APP_NAME}.app for local execution ==="
codesign --force --deep --sign - "${APP_BUNDLE}"

echo "=========================================================="
echo " Build & Packaging Complete!"
echo " App location: ${APP_BUNDLE}"
echo " To run:"
echo "   open ${APP_BUNDLE}"
echo "=========================================================="
