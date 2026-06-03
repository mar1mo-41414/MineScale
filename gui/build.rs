// Embed Windows PE VS_VERSION_INFO into the .exe so the file shows up
// properly in Explorer Properties and gets parsed by winget /
// wingetcreate / Microsoft Store Submission tools.
//
// This only runs for Windows targets; on any other target it's a no-op.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }

    let mut res = winresource::WindowsResource::new();

    // When cross-compiling from non-Windows (our build-all.sh does this
    // via MinGW), tell winresource which windres + toolchain prefix to use.
    if !cfg!(target_os = "windows") {
        // /opt/homebrew/bin/x86_64-w64-mingw32-windres is in PATH on our
        // build host.  The crate accepts either an explicit path or a
        // toolkit prefix; we set the prefix to match MinGW's convention.
        res.set_windres_path("x86_64-w64-mingw32-windres");
        res.set_ar_path("x86_64-w64-mingw32-ar");
    }

    let version = env!("CARGO_PKG_VERSION");

    res.set("ProductName",      "MineScale");
    res.set("FileDescription",  "MineScale-Java — Minecraft Java world sharing");
    res.set("CompanyName",      "mar1mo-41414");
    res.set("LegalCopyright",   "MIT License — Copyright (c) 2025 mar1mo-41414");
    res.set("OriginalFilename", "mc-share-gui.exe");
    res.set("InternalName",     "mc-share-gui");
    res.set("ProductVersion",   version);
    res.set("FileVersion",      version);

    if let Err(e) = res.compile() {
        // Don't hard-fail the build if windres is missing on a dev
        // machine — produce a warning and continue without VS_VERSION_INFO.
        println!("cargo:warning=skipping Windows resource embed: {}", e);
    }
}
