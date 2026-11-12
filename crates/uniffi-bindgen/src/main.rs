//! Generates the Swift sources, C headers, and modulemap for the macOS app
//! from the compiled `audio-switch-ffi` library. Run by
//! `scripts/build-macos-core.sh`.

#[cfg(target_os = "macos")]
fn main() {
    uniffi::uniffi_bindgen_swift();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Swift bindings are only generated on macOS.");
    std::process::exit(1);
}
