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

    // The release workflow signs with a key from GitHub secrets (docs/releasing.md). Without
    // KITE_KEYSTORE_FILE, assembleRelease makes an unsigned APK.
    val keystore = System.getenv("KITE_KEYSTORE_FILE")
    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("KITE_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("KITE_KEY_ALIAS")
                keyPassword = System.getenv("KITE_KEY_PASSWORD")
            }
        }
    }
    buildTypes {
        getByName("release") {
            if (keystore != null) signingConfig = signingConfigs.getByName("release")
        }
    }
}

dependencies {
    // The uniffi bindings call into libkite_ffi.so through JNA.
    implementation("net.java.dev.jna:jna:5.19.1@aar")
}
