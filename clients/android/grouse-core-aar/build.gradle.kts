plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

// The uniffi-generated Kotlin bindings for BOTH crates (grouse-core + grouse-roam-core)
// plus the native cdylib, packaged as one Android library module.
//
// ONE .so ships: libgrouse_core.so. grouse-roam-core is an rlib dependency of grouse-core,
// so the roam transport links statically into it and exports its uniffi symbols from there
// — the generated uniffi.grouse_roam_core package loads "grouse_core", not a library of its
// own. Regenerate and stage everything with `just android-libs`.
android {
    namespace = "dev.grouse.grousecore"
    compileSdk = 34

    defaultConfig {
        minSdk = 26
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
}

// uniffi's Kotlin bindings call the native libs through JNA (Native.register); the @aar
// variant carries the Android-native jnidispatch and skips the desktop ones.
dependencies {
    implementation("net.java.dev.jna:jna:5.14.0@aar")
}
