fn main() {
    println!("cargo:rustc-check-cfg=cfg(apple_intelligence_sdk)");

    let vendor = "vendor/libmobi";
    println!("cargo:rerun-if-changed={vendor}");
    println!("cargo:rerun-if-changed=src/formats/mobi_bridge.c");

    let mut build = cc::Build::new();
    build
        .include(vendor)
        .file("src/formats/mobi_bridge.c")
        .define("PACKAGE_VERSION", "\"0.12\"")
        .define("USE_XMLWRITER", None)
        .define("USE_MINIZ", None)
        .define("MINIZ_NO_STDIO", None)
        .define("MINIZ_NO_ZLIB_COMPATIBLE_NAMES", None)
        .define("MINIZ_NO_TIME", None)
        .define("MINIZ_NO_ARCHIVE_APIS", None)
        .flag_if_supported("-std=c99")
        .warnings(false);
    for source in [
        "buffer",
        "compression",
        "debug",
        "index",
        "memory",
        "meta",
        "miniz",
        "opf",
        "parse_rawml",
        "read",
        "structure",
        "util",
        "write",
        "xmlwriter",
    ] {
        build.file(format!("{vendor}/{source}.c"));
    }
    build.compile("cr_mobi");

    build_apple_intelligence_bridge();
}

fn build_apple_intelligence_bridge() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    println!("cargo:rerun-if-changed=src/ai/apple_bridge.swift");

    let sdk_version = std::process::Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-version"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .split('.')
                .next()
                .and_then(|major| major.parse::<u32>().ok())
        });

    if !sdk_version.is_some_and(|major| major >= 26) {
        return;
    }

    let Some(sdk_path) = std::process::Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|path| !path.is_empty())
    else {
        return;
    };

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let obj_path = out_dir.join("apple_bridge.o");
    let lib_path = out_dir.join("libcr_apple_ai.a");

    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86_64",
        _ => "arm64",
    };
    let deployment = std::env::var("MACOSX_DEPLOYMENT_TARGET")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "14.0".into());
    let target = format!("{arch}-apple-macosx{deployment}");

    let compiled = std::process::Command::new("xcrun")
        .args([
            "swiftc",
            "-emit-object",
            "-parse-as-library",
            "-O",
            "-sdk",
            &sdk_path,
            "-target",
            &target,
            "-o",
        ])
        .arg(&obj_path)
        .arg("src/ai/apple_bridge.swift")
        .status();

    let Ok(status) = compiled else {
        println!("cargo:warning=xcrun swiftc unavailable; Apple Intelligence bridge skipped");
        return;
    };
    if !status.success() {
        println!("cargo:warning=swiftc failed to build Apple Intelligence bridge; continuing without it");
        return;
    }

    let archived = std::process::Command::new("ar")
        .args(["rcs"])
        .arg(&lib_path)
        .arg(&obj_path)
        .status();

    if !matches!(archived, Ok(status) if status.success()) {
        println!("cargo:warning=ar failed to archive Apple Intelligence bridge; continuing without it");
        return;
    }

    println!("cargo:rustc-cfg=apple_intelligence_sdk");
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=cr_apple_ai");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-arg=-Wl,-weak_framework,FoundationModels");
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
}
