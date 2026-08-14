plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

// The uniffi-generated Kotlin bindings for BOTH crates (grouse-core + grouse-roam-core)
// plus their native cdylibs, packaged as one Android library module. The bindings live in
// core/grouse-core/bindings/kotlin and the .so files in the crates' target dirs; they are
// staged here (see the monorepo README for the regen/repack step).
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
