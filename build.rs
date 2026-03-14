fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "android" {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
        let abi = match target_arch.as_str() {
            "aarch64" => "arm64-v8a",
            "arm" => "armeabi-v7a",
            "x86_64" => "x86_64",
            "x86" => "x86",
            _ => panic!("Unsupported Android arch: {}", target_arch),
        };
        println!("cargo:rustc-link-search=native={}/libs/{}", manifest_dir, abi);
        println!("cargo:rustc-link-lib=camera2ndk");
        println!("cargo:rustc-link-lib=mediandk");
        println!("cargo:rustc-link-lib=pipewire-0.3");
    }
}
