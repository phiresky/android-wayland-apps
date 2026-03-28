fn main() {
    // Only build for Android targets
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "android" {
        // On non-Android, just provide the header path for bindgen/IDE usage
        println!(
            "cargo:include={}",
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("csrc")
                .display()
        );
        return;
    }

    let mut build = cc::Build::new();
    build
        .file("csrc/gbm_ahb.c")
        .include("csrc")
        .warnings(true)
        // Android NDK flags
        .flag("-std=c17")
        .flag("-fvisibility=hidden")
        .flag("-Wno-unused-parameter");

    build.compile("gbm_ahb");

    // Link Android system libraries
    println!("cargo:rustc-link-lib=android");
    println!("cargo:rustc-link-lib=log");
    println!("cargo:rustc-link-lib=dl");

    // Re-run if C source changes
    println!("cargo:rerun-if-changed=csrc/gbm_ahb.c");
    println!("cargo:rerun-if-changed=csrc/gbm_ahb.h");
}
