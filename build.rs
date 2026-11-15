use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn copy_if_changed(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source_bytes = fs::read(source)?;
    if fs::read(destination).ok().as_deref() != Some(source_bytes.as_slice()) {
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn configure_windows_libarchive() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }

    println!("cargo:rerun-if-env-changed=LIBARCHIVE_ROOT");
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-env-changed=VCPKG_DEFAULT_TRIPLET");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let explicit = env::var_os("LIBARCHIVE_ROOT").map(PathBuf::from);
    let triplet = env::var("VCPKG_DEFAULT_TRIPLET").unwrap_or_else(|_| {
        match arch.as_str() {
            "x86_64" => "x64-windows",
            "aarch64" => "arm64-windows",
            "x86" => "x86-windows",
            _ => "unknown-windows",
        }
        .to_owned()
    });
    let mut roots = Vec::new();
    if let Some(root) = &explicit {
        roots.push(root.clone());
    }
    if let Some(root) = env::var_os("VCPKG_ROOT") {
        roots.push(PathBuf::from(root).join("installed").join(triplet));
    }
    if arch == "x86_64" {
        roots.push(
            PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
                .join("target/windows-tools/libarchive-static"),
        );
    }
    let find_lib = |root: &Path| {
        ["libarchive.lib", "archive.lib"]
            .iter()
            .map(|name| root.join("lib").join(name))
            .find(|p| p.is_file())
    };
    if let Some(root) = explicit.as_ref() {
        assert!(
            find_lib(root).is_some(),
            "LIBARCHIVE_ROOT must contain lib/archive.lib or lib/libarchive.lib: {}",
            root.display()
        );
    }
    let Some((root, library)) = roots
        .into_iter()
        .find_map(|root| find_lib(&root).map(|lib| (root, lib)))
    else {
        println!("cargo:warning=libarchive not found automatically; set LIBARCHIVE_ROOT or VCPKG_ROOT, or add archive.lib to LIB");
        return;
    };
    println!("cargo:rerun-if-changed={}", library.display());
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::copy(&library, out.join("archive.lib"))
        .expect("failed to prepare libarchive import library");
    println!("cargo:rustc-link-search=native={}", out.display());

    // Cargo runs the executable from the profile directory. Keep all DLLs from
    // the selected distribution beside it, including libarchive's dependencies.
    let bin = root.join("bin");
    println!("cargo:rerun-if-changed={}", bin.display());
    if bin.is_dir() {
        let profile = out.ancestors().nth(3).expect("unexpected Cargo OUT_DIR");
        for entry in fs::read_dir(&bin).expect("failed to read libarchive DLL directory") {
            let entry = entry.expect("failed to read libarchive DLL entry");
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"))
            {
                copy_if_changed(&path, &profile.join(entry.file_name()))
                    .expect("failed to copy libarchive runtime DLL");
            }
        }
        let archive = profile.join("archive.dll");
        let libarchive = profile.join("libarchive.dll");
        if !bin.join("archive.dll").is_file() && bin.join("libarchive.dll").is_file() {
            copy_if_changed(&libarchive, &archive).expect("failed to create archive.dll alias");
        } else if bin.join("archive.dll").is_file() && !bin.join("libarchive.dll").is_file() {
            copy_if_changed(&archive, &libarchive).expect("failed to create libarchive.dll alias");
        }
    } else {
        println!("cargo:warning=libarchive bin directory not found; runtime DLLs must be available on PATH");
    }
}

fn main() {
    configure_windows_libarchive();
    tauri_build::build();
}
