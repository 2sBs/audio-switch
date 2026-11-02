# Contributing

Audio Switch has a Windows tray app and an in-development macOS menu bar app.
Both use the shared Rust core in `crates/audio-switch-core`. Windows development
requires the stable Rust MSVC toolchain and Visual Studio Build Tools for C++
desktop development. macOS development requires Xcode 16 or newer and Rust from
rustup.

Before opening a pull request on Windows, run:

```powershell
cargo fmt --all -- --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
```

On macOS, run the same checks for the crates that build there, as listed in the
README.

Keep changes focused. Update current documentation when behavior changes and add
tests for routing state, configuration compatibility, or IPC behavior where a
regression would be hard to spot manually.

Preserve these project boundaries:

- Routing uses configurable Primary and Secondary roles, not hard-coded device
  names.
- Audio Switch does not bundle or download a third-party virtual audio driver.
- The Stream Deck plugin controls the tray app and never launches its own copy.
- Saved routes do not silently fall back when a configured device disconnects.

Do not commit generated files from `target\`, `dist\`, or plugin packaging. If a
change affects setup, Settings, the tray or menu bar menu, or Stream Deck
feedback, describe the manual check you performed on each affected platform in
the pull request.

Bug reports may include logs or settings, but those files can contain audio
device names and endpoint IDs. Remove private device information before posting
them publicly.

By contributing, you agree that your contribution is licensed under the MIT
License.
