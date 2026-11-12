plugins {
    id("com.android.application")
}

android {
    namespace = "io.github.heidaridev.kite"
    compileSdk = 36

    defaultConfig {
        applicationId = "io.github.heidaridev.kite"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.0.0"
        ndk {
            abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64")
        }
    }
}

dependencies {
    // The uniffi bindings call into libkite_ffi.so through JNA.
    implementation("net.java.dev.jna:jna:5.19.1@aar")
}
