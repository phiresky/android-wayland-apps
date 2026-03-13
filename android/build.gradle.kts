plugins {
    id("com.android.application") version "9.1.0"
}

android {
    namespace = "io.github.phiresky.wayland_android"
    compileSdk = 35

    defaultConfig {
        applicationId = "io.github.phiresky.wayland_android"
        minSdk = 34
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    sourceSets {
        getByName("main") {
            // Point jniLibs at both the cargo output and the prebuilt libs
            jniLibs.srcDirs(
                "../jniLibs",  // we'll assemble everything here
            )
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    packaging {
        jniLibs {
            useLegacyPackaging = true  // required for extractNativeLibs=true
            keepDebugSymbols += "**/*.so"
        }
    }
}

dependencies {
    implementation("androidx.core:core:1.16.0")
    implementation("androidx.swiperefreshlayout:swiperefreshlayout:1.1.0")
}

// Copy all native .so files into jniLibs before build
tasks.register<Copy>("assembleNativeLibs") {
    // Cargo ndk output
    from("../target/aarch64-linux-android/debug/libandroid_wayland_launcher.so")
    // Prebuilt libs (xkbcommon, proot, proot_loader)
    from("../libs/arm64-v8a/") {
        include("*.so")
    }
    into("../jniLibs/arm64-v8a")
}

tasks.named("preBuild") {
    dependsOn("assembleNativeLibs")
}