#!/usr/bin/env bash
# Builds the Rust core for the macOS app and regenerates its Swift bindings in
# macos/Generated. Xcode runs this before compiling Swift; it also works on its
# own. Uses the release profile when Xcode's CONFIGURATION is Release.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
generated="$root/macos/Generated"
# Xcode does not inherit the login shell's PATH.
export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"

profile=debug
cargo_profile=()
if [[ "${CONFIGURATION:-Debug}" == "Release" ]]; then
    profile=release
    cargo_profile=(--release)
fi

cd "$root"
cargo build --locked -p audio-switch-ffi ${cargo_profile[@]+"${cargo_profile[@]}"}

staging="$root/target/swift-bindings"
mkdir -p "$staging"
cargo run --locked --quiet -p uniffi-bindgen -- \
    "target/$profile/libaudio_switch_ffi.dylib" "$staging" \
    --swift-sources --headers --modulemap \
    --module-name AudioSwitchFFI --modulemap-filename module.modulemap \
    --config macos/uniffi.toml
cp "target/$profile/libaudio_switch_ffi.a" "$staging/"

# Only touch files whose contents changed, so Xcode does not recompile the
# bindings on every build.
mkdir -p "$generated"
rsync --recursive --checksum --delete "$staging/" "$generated/"
