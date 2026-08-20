fn main() {
    println!("cargo:rerun-if-changed=assets/tray/primary.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/tray/primary.ico")
            .compile()
            .expect("embedding the Audio Switch application icon");
    }
}
