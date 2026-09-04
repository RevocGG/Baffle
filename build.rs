//! Build script: embeds the Windows resources (app icon, version info,
//! manifest) into the executable by compiling `assets/baffle.rc` with
//! `windres` and linking the resulting COFF object into the final binary.
//!
//! windres (with `as`) is bundled in `tools/localbin`; the build script finds
//! it there even if it is not on PATH. On non-Windows targets this is a no-op.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/baffle.rc");
    println!("cargo:rerun-if-changed=assets/baffle.manifest");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    if !cfg!(target_os = "windows") {
        return;
    }
    // Skip when cross-compiling (windres would need to match the target).
    let target = std::env::var("TARGET").unwrap_or_default();
    let host = std::env::var("HOST").unwrap_or_default();
    if target != host {
        println!("cargo:warning=baffle build.rs: cross-compiling ({target}); skipping resource embedding");
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let assets = manifest_dir.join("assets");
    let rc = assets.join("baffle.rc");
    let res_obj = out_dir.join("baffle.res.o");

    let windres = find_tool(&manifest_dir, "windres.exe");

    // windres spawns `gcc -E` for preprocessing; make sure it can find the
    // bundled gcc (tools/mingw64/bin) even when it is not on PATH.
    let mingw_bin = manifest_dir.join("tools/mingw64/bin");
    let local_bin = manifest_dir.join("tools/localbin");
    let mut path_entries = Vec::new();
    if mingw_bin.is_dir() {
        path_entries.push(mingw_bin);
    }
    if local_bin.is_dir() {
        path_entries.push(local_bin);
    }
    if let Some(p) = std::env::var_os("PATH") {
        path_entries.extend(std::env::split_paths(&p));
    }
    let augmented_path = std::env::join_paths(path_entries).unwrap();

    let status = Command::new(&windres)
        .arg("-I")
        .arg(&assets) // resolve "icon.ico" / "baffle.manifest" from assets/
        .arg(&rc)
        .arg("-O")
        .arg("coff")
        .arg("-o")
        .arg(&res_obj)
        .env("PATH", &augmented_path)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", windres.display()));
    if !status.success() {
        panic!("windres failed to compile {}", rc.display());
    }

    // Prefer the optional local import library when present, but allow a
    // normal system MinGW installation to provide it on fresh clones.
    let mingw_libs = manifest_dir.join("tools/mingw-libs");
    if mingw_libs.is_dir() {
        println!("cargo:rustc-link-search=native={}", mingw_libs.display());
    }

    // Link the resource object into every bin target.
    println!("cargo:rustc-link-arg-bins={}", res_obj.display());
}

/// Locate a tool: explicit env override (`BAFFLE_WINDRES`), then PATH, then the
/// project-local `tools/localbin` and `tools/mingw64/bin` bundles.
fn find_tool(manifest_dir: &Path, name: &str) -> PathBuf {
    if let Ok(p) = std::env::var("BAFFLE_WINDRES") {
        let p = PathBuf::from(p);
        if p.exists() {
            return p;
        }
    }
    // Already on PATH?
    if Command::new(name).arg("--version").output().is_ok() {
        return PathBuf::from(name);
    }
    for rel in ["tools/localbin", "tools/mingw64/bin"] {
        let p = manifest_dir.join(rel).join(name);
        if p.exists() {
            return p;
        }
    }
    panic!("{name} not found; install MinGW-w64/binutils or add tools/localbin");
}
