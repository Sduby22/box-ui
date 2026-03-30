#!/usr/bin/env bash
set -euo pipefail

VERSION=""
TARGET=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --target)  TARGET="$2";  shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

if [[ -z "$VERSION" ]]; then
    echo "Usage: $0 --version <version> [--target <triple>]"
    exit 1
fi

if [[ -z "$TARGET" ]]; then
    TARGET=$(rustc -vV | sed -n 's/^host: //p')
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DIST_DIR="$PROJECT_DIR/dist"

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

echo "Building box-ui v$VERSION for $TARGET..."
cargo build --release --target "$TARGET"

case "$TARGET" in
    *-windows-*)
        ARTIFACT_NAME="box-ui-v${VERSION}-windows-x86_64.zip"
        cp "$PROJECT_DIR/target/$TARGET/release/box-ui.exe" "$DIST_DIR/box-ui.exe"
        (cd "$DIST_DIR" && 7z a -tzip "$ARTIFACT_NAME" box-ui.exe > /dev/null)
        rm "$DIST_DIR/box-ui.exe"
        echo "Created $DIST_DIR/$ARTIFACT_NAME"
        ;;

    *-apple-darwin)
        ARCH="${TARGET%%-*}"
        ARTIFACT_NAME="box-ui-v${VERSION}-macos-${ARCH}.dmg"

        # Generate .icns from 1024.png
        ICONSET_DIR=$(mktemp -d)/AppIcon.iconset
        mkdir -p "$ICONSET_DIR"
        ICON_SRC="$PROJECT_DIR/assets/icons/1024.png"

        sips -z 16 16     "$ICON_SRC" --out "$ICONSET_DIR/icon_16x16.png"      > /dev/null
        sips -z 32 32     "$ICON_SRC" --out "$ICONSET_DIR/icon_16x16@2x.png"   > /dev/null
        sips -z 32 32     "$ICON_SRC" --out "$ICONSET_DIR/icon_32x32.png"      > /dev/null
        sips -z 64 64     "$ICON_SRC" --out "$ICONSET_DIR/icon_32x32@2x.png"   > /dev/null
        sips -z 128 128   "$ICON_SRC" --out "$ICONSET_DIR/icon_128x128.png"    > /dev/null
        sips -z 256 256   "$ICON_SRC" --out "$ICONSET_DIR/icon_128x128@2x.png" > /dev/null
        sips -z 256 256   "$ICON_SRC" --out "$ICONSET_DIR/icon_256x256.png"    > /dev/null
        sips -z 512 512   "$ICON_SRC" --out "$ICONSET_DIR/icon_256x256@2x.png" > /dev/null
        sips -z 512 512   "$ICON_SRC" --out "$ICONSET_DIR/icon_512x512.png"    > /dev/null
        cp "$ICON_SRC"               "$ICONSET_DIR/icon_512x512@2x.png"

        ICNS_PATH="$DIST_DIR/AppIcon.icns"
        iconutil -c icns "$ICONSET_DIR" -o "$ICNS_PATH"
        rm -rf "$(dirname "$ICONSET_DIR")"

        # Create .app bundle
        APP_DIR="$DIST_DIR/Box UI.app"
        mkdir -p "$APP_DIR/Contents/MacOS"
        mkdir -p "$APP_DIR/Contents/Resources"

        cp "$PROJECT_DIR/target/$TARGET/release/box-ui" "$APP_DIR/Contents/MacOS/box-ui"
        cp "$ICNS_PATH" "$APP_DIR/Contents/Resources/AppIcon.icns"
        sed "s/__VERSION__/$VERSION/g" "$PROJECT_DIR/assets/macos/Info.plist.template" \
            > "$APP_DIR/Contents/Info.plist"
        rm "$ICNS_PATH"

        # Create DMG
        hdiutil create -volname "Box UI" \
            -srcfolder "$APP_DIR" \
            -ov -format UDZO \
            "$DIST_DIR/$ARTIFACT_NAME"
        rm -rf "$APP_DIR"
        echo "Created $DIST_DIR/$ARTIFACT_NAME"
        ;;

    *)
        echo "Unsupported target: $TARGET"
        exit 1
        ;;
esac
