//! Generates the application icon and embeds it (plus version info) into the
//! Windows executable.

#[path = "src/icon.rs"]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-env=METALMD_VERSION={}", version());

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
            .set("FileDescription", "MetalMD - Markdown viewer")
            .set("ProductName", "MetalMD")
            .set("LegalCopyright", "Copyright (C) 2026 Fan4_Metal");
        if let Err(e) = res.compile() {
            // A missing resource compiler must not break the build.
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}

/// Development builds include their Git revision so builds between releases
/// can be told apart. A release version is shown exactly as Cargo specifies it.
fn version() -> String {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    if !version.contains('-') {
        return version;
    }
    // Shallow checkouts may have no tags; --always falls back to the short
    // hash. Do not use --dirty: source-only edits do not rerun this script.
    match git(&["describe", "--tags", "--always", "--match", "v[0-9]*"]) {
        Some(describe) => {
            watch_git();
            format!("{version} ({describe})")
        }
        None => version,
    }
}

/// Refresh the displayed revision on commits, branch switches and tag changes.
/// Separate Git/common directories also support linked worktrees.
fn watch_git() {
    let Some(dirs) = git(&["rev-parse", "--git-dir", "--git-common-dir"]) else {
        return;
    };
    let mut dirs = dirs.lines().map(std::path::PathBuf::from);
    let (Some(git_dir), Some(common)) = (dirs.next(), dirs.next()) else {
        return;
    };
    let head = git_dir.join("HEAD");
    let mut paths = vec![
        head.clone(),
        common.join("packed-refs"),
        common.join("refs").join("tags"),
    ];
    if let Some(branch) = std::fs::read_to_string(&head).ok().and_then(|head| {
        head.strip_prefix("ref: ")
            .map(|name| name.trim().to_owned())
    }) {
        paths.push(common.join(branch));
    }
    // Missing paths would make Cargo rerun the script on every build.
    for path in paths.iter().filter(|path| path.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// Trimmed Git output, or None when Git or repository metadata is unavailable.
fn git(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !text.is_empty()).then_some(text)
}
