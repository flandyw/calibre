fn main() {
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
}
