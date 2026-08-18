plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
}

android {
    namespace = "id.gauvin.grouse"
    compileSdk = 34

    defaultConfig {
        applicationId = "id.gauvin.grouse"
        minSdk = 26
        targetSdk = 34
        // The only native code is libgrouse_core.so (the roam transport links
        // statically into it — there is no separate grouse-roam-core .so).
        // arm64-v8a for devices, x86_64 for emulators; see `just android-libs`.
        ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
        // MUST be bumped on every sideloaded build. It sat at 1 through many rebuilds, and a
        // same-versionCode install is a reinstall Android may silently skip or refuse -- the
        // installer reports success while the old APK stays in place, so fixes appear not to
        // work and get re-debugged from scratch. versionName carries the date for the same
        // reason: so "which build is this?" is answerable from the About/app-info screen.
        versionCode = 74
        versionName = "0.68-20260817"
    }

    // Release signing, used ONLY when the four properties below are supplied (CI sets them from
    // repo secrets; locally they come from ~/.android/grouse-release.env). Absent them the block
    // is not created at all and `release` falls back to debug signing — see below.
    //
    // NEVER commit the keystore or these values. Losing the keystore is unrecoverable: Android
    // will not accept an update signed by a different key, so every installed user would have to
    // uninstall and lose local state.
    val ksPath = (findProperty("grouse.keystore") ?: System.getenv("GROUSE_KEYSTORE"))?.toString()
    val ksStorePass = (findProperty("grouse.storePassword") ?: System.getenv("GROUSE_STORE_PASSWORD"))?.toString()
    val ksAlias = (findProperty("grouse.keyAlias") ?: System.getenv("GROUSE_KEY_ALIAS"))?.toString()
    val ksKeyPass = (findProperty("grouse.keyPassword") ?: System.getenv("GROUSE_KEY_PASSWORD"))?.toString()
    val hasReleaseSigning = !ksPath.isNullOrBlank() && file(ksPath).exists() &&
        !ksStorePass.isNullOrBlank() && !ksAlias.isNullOrBlank() && !ksKeyPass.isNullOrBlank()

    signingConfigs {
        if (hasReleaseSigning) {
            create("release") {
                storeFile = file(ksPath!!)
                storePassword = ksStorePass
                keyAlias = ksAlias
                keyPassword = ksKeyPass
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // With a real keystore: a distributable build. Without one: sign with the DEBUG
            // keystore so the release APK installs OVER the debug app (same signature, no
            // uninstall) — the personal sideload path, unchanged. What matters either way is
            // isDebuggable=false, which is what removes Compose's debug-mode jank.
            //
            // The debug keystore is fine for your own phone and NOT fine for distribution: it is
            // `androiddebugkey`/`android`, shipped with every SDK, so anyone can sign an APK that
            // Android will accept as an update over it. Published builds must use the real key.
            signingConfig = if (hasReleaseSigning) signingConfigs.getByName("release")
                            else signingConfigs.getByName("debug")
        }
    }
    // JNA (the roam transport's FFI) loads native libs via System.loadLibrary —
    // that needs them EXTRACTED to nativeLibraryDir at install. The AGP default
    // (extractNativeLibs=false) left the libs in the APK and every uniffi call
    // died with an opaque class-init error; JNA's APK-resource fallback looks
    // under android-aarch64/, which AGP's lib/arm64-v8a/ layout never matches.
    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    buildFeatures {
        compose = true
    }
}

// The UnifiedPush connector transitively pulls kotlin-stdlib 2.3.0, whose metadata this project's
// Kotlin 2.0.20 compiler can't read. Pin the stdlib to our compiler's version.
configurations.all {
    resolutionStrategy {
        force("org.jetbrains.kotlin:kotlin-stdlib:2.0.20")
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.activity:activity-compose:1.9.2")
    implementation(platform("androidx.compose:compose-bom:2024.09.02"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.6")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.6")
    implementation("androidx.lifecycle:lifecycle-process:2.8.6")
    implementation("androidx.navigation:navigation-compose:2.8.0")
    implementation("androidx.security:security-crypto:1.1.0-alpha06")
    implementation("androidx.biometric:biometric:1.1.0")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.7.1")
    // Markdown rendering for agent output (headers, bold, lists, fenced code).
    implementation("com.halilibo.compose-richtext:richtext-commonmark:0.20.0")
    implementation("com.halilibo.compose-richtext:richtext-ui-material3:0.20.0")
    // UnifiedPush: receive server-pushed briefings/alerts via a distributor (NextPush) — no FCM,
    // no always-on socket. Exclude the connector's JVM tink; security-crypto needs tink-android
    // (Android Keystore), so keep only that and bump it high enough for the connector's classes to
    // resolve — otherwise the two Tink artifacts collide (duplicate classes).
    implementation("org.unifiedpush.android:connector:3.3.3") {
        exclude(group = "com.google.crypto.tink", module = "tink")
    }
    implementation("com.google.crypto.tink:tink-android:1.16.0")
    // grouse core + roam transport: uniffi Kotlin bindings over the locally built
    // cdylibs (arm64-v8a), packaged by the :grouse-core-aar module.
    implementation(project(":grouse-core-aar"))
    // QR pairing for roam hosts: CameraX preview + zxing core (pure Java — the
    // ML Kit barcode engine was ~19 MB of native libbarhopper across 4 ABIs for
    // one QR decode; zxing is ~700 KB with zero natives).
    implementation("androidx.camera:camera-camera2:1.3.4")
    implementation("androidx.camera:camera-lifecycle:1.3.4")
    implementation("androidx.camera:camera-view:1.3.4")
    implementation("com.google.zxing:core:3.5.3")

    // JVM unit tests: parsers and wire framing only — no Android framework, no Robolectric.
    // Defends the ACP contracts that have bitten repeatedly (casing, session_info_update keys,
    // extension DTO shapes, _meta.client). See app/src/test/java/id/gauvin/grouse/.
    testImplementation("junit:junit:4.13.2")
    testImplementation("com.squareup.okhttp3:mockwebserver:4.12.0")
}
