//! Generates the application icon and embeds it (plus version info) into the
//! Windows executable.

#[path = "src/icon.rs"]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // The icon as raw RGBA for the window icon, the start page and About
    // (see `main::embedded_icon`), so that nothing is rasterised at start-up.
    for size in [64u32, 128, 256] {
        std::fs::write(
            out_dir.join(format!("app_icon_{size}.rgba")),
            icon::rgba(size),
        )
        .expect("write an icon");
    }

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico_path = out_dir.join("app.ico");
    std::fs::write(&ico_path, icon::ico(&icon::ICO_SIZES)).expect("write app.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico_path.to_str().unwrap())
            .set("FileDescription", "qmarkdown - Markdown viewer")
            .set("ProductName", "qmarkdown")
            .set("LegalCopyright", "Copyright (C) 2026 Fan4_Metal");
        if let Err(e) = res.compile() {
            // A missing resource compiler must not break the build.
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}
