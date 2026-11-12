#!/usr/bin/env bash
# Packages the Stream Deck plugin on macOS. The package runs on both platforms,
# so the Windows controller must already be in the plugin folder; build it with
# scripts/package-stream-deck.ps1 or copy it from a Windows build.
#
# Usage: scripts/package-stream-deck.sh [version]
set -euo pipefail

repository="$(cd "$(dirname "$0")/.." && pwd)"
plugin="$repository/stream-deck/com.2sbsi.audio-switch.sdPlugin"
distribution="$repository/dist"
build_target="$repository/target/stream-deck-package"
# The macOS app is arm64-only, so the controller is too.
rust_target="aarch64-apple-darwin"
version="${1:-}"

if [[ -z "$version" ]]; then
    version="$(cargo metadata --format-version 1 --no-deps | node -e '
        const metadata = JSON.parse(require("fs").readFileSync(0, "utf8"));
        const application = metadata.packages.find((p) => p.name === "audio-switch");
        if (!application) throw new Error("Could not find the audio-switch Cargo package.");
        console.log(application.version);
    ')"
fi
if [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    version="$version.0"
elif [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Invalid version: $version" >&2
    exit 1
fi

if [[ ! -f "$plugin/audio-switch-stream-deck.exe" ]]; then
    echo "Missing $plugin/audio-switch-stream-deck.exe; copy in the Windows controller first." >&2
    exit 1
fi

cargo build --locked --release --bin audio-switch-stream-deck \
    --target "$rust_target" --target-dir "$build_target"
cp "$build_target/$rust_target/release/audio-switch-stream-deck" "$plugin/audio-switch-stream-deck"

# Stream Deck needs a PNG plugin icon; render it from the committed SVG.
sips -s format png "$plugin/imgs/plugin.svg" -z 144 144 --out "$plugin/imgs/plugin.png" >/dev/null
sips -s format png "$plugin/imgs/plugin.svg" -z 288 288 --out "$plugin/imgs/plugin@2x.png" >/dev/null

streamdeck validate "$plugin" --no-update-check
mkdir -p "$distribution"
streamdeck pack "$plugin" --output "$distribution" --version "$version" --force --no-update-check
