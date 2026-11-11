#!/usr/bin/env bash
set -e

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

APP_NAME="MicMirror"
TARGET_DIR="target/release"
APP_BUNDLE="${TARGET_DIR}/${APP_NAME}.app"
DMG_NAME="${APP_NAME}-macos-arm64.dmg"
DMG_PATH="${TARGET_DIR}/${DMG_NAME}"
DMG_STAGING="${TARGET_DIR}/dmg_staging"

# 1. Ensure the .app bundle is built
if [ ! -d "${APP_BUNDLE}" ]; then
    echo "=== Building ${APP_NAME}.app first ==="
    ./scripts/build_app.sh
fi

echo "=== Preparing DMG staging directory ==="
rm -rf "${DMG_STAGING}" "${DMG_PATH}" "${DMG_PATH}.tmp.dmg"
mkdir -p "${DMG_STAGING}"

# Copy .app bundle into staging
cp -R "${APP_BUNDLE}" "${DMG_STAGING}/"

# Create symlink to /Applications for standard drag-and-drop installer UX
ln -s /Applications "${DMG_STAGING}/Applications"

echo "=== Creating DMG image ==="
if command -v create-dmg >/dev/null 2>&1; then
    echo "Using create-dmg for styled installer window..."
    create-dmg \
        --volname "${APP_NAME} Installer" \
        --window-pos 200 120 \
        --window-size 600 400 \
        --icon-size 120 \
        --icon "${APP_NAME}.app" 150 190 \
        --app-drop-link 450 190 \
        --hide-extension "${APP_NAME}.app" \
        --no-internet-enable \
        "${DMG_PATH}" \
        "${DMG_STAGING}" || {
            echo "create-dmg encountered an issue, falling back to hdiutil..."
            rm -rf "${DMG_PATH}"
            hdiutil create -volname "${APP_NAME}" -srcfolder "${DMG_STAGING}" -ov -format UDZO "${DMG_PATH}"
        }
else
    echo "create-dmg not found, creating clean DMG using macOS hdiutil..."
    hdiutil create -volname "${APP_NAME}" -srcfolder "${DMG_STAGING}" -ov -format UDZO "${DMG_PATH}"
fi

# Clean up staging
rm -rf "${DMG_STAGING}"

echo "=========================================================="
echo " DMG Creation Complete!"
echo " DMG location: ${DMG_PATH}"
echo "=========================================================="

