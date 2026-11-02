fn main() {
    #[cfg(windows)]
    {
        const ICON: &str = "../audio-switch-windows/assets/tray/primary.ico";
        println!("cargo:rerun-if-changed={ICON}");
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            winresource::WindowsResource::new()
                .set_icon(ICON)
                .compile()
                .expect("embedding the Stream Deck controller icon");
        }
    }
}
