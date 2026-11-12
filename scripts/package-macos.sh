#!/bin/bash
# Builds the Release macOS app and zips it into dist/ for distribution.
# Usage: scripts/package-macos.sh [version]
# The version defaults to the Cargo version of audio-switch-ffi. The app is
# ad-hoc signed (see macos/Base.xcconfig), so it is not notarized.
set -euo pipefail

export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"
repository="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repository"

version="${1:-}"
if [[ -z "$version" ]]; then
    version="$(cargo metadata --format-version 1 --no-deps |
        jq -r '.packages[] | select(.name == "audio-switch-ffi") | .version')"
fi
app_version="$(sed -n 's/^MARKETING_VERSION = //p' macos/Base.xcconfig)"
if [[ "$app_version" != "$version" ]]; then
    echo "macOS app version $app_version does not match $version." >&2
    exit 1
fi

symroot="$repository/target/xcode"
xcodebuild -project macos/AudioSwitch.xcodeproj -target AudioSwitch \
    -configuration Release SYMROOT="$symroot" build
app="$symroot/Release/Audio Switch.app"
codesign --verify --strict "$app"

mkdir -p dist
archive="dist/audio-switch-$version-macos-arm64.zip"
# ditto keeps the bundle's symlinks, permissions and signature intact.
ditto -c -k --sequesterRsrc --keepParent "$app" "$archive"
echo "Wrote $archive"
