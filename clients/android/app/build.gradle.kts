plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
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
        versionCode = 78
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

// The UnifiedPush connector transitively pulls kotlin-stdlib 2.3.0, whose metadata the 2.0.20
// compiler can't read (binary metadata 2.3.0 vs expected 2.0.0). The version catalog (A-13)
// centralizes OUR versions, but removing this force re-broke resolution exactly as the old
// comment warned — so it is kept. Revisit when the Kotlin compiler moves past 2.3.0 metadata.
configurations.all {
    resolutionStrategy {
        force("org.jetbrains.kotlin:kotlin-stdlib:2.0.20")
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.compose.material3)
    implementation(libs.compose.material.icons.extended)
    implementation(libs.lifecycle.viewmodel.compose)
    implementation(libs.lifecycle.runtime.compose)
    implementation(libs.lifecycle.process)
    implementation(libs.navigation.compose)
    implementation(libs.security.crypto)
    implementation(libs.androidx.biometric)
    implementation(libs.okhttp)
    implementation(libs.kotlinx.serialization.json)
    // Markdown rendering for agent output (headers, bold, lists, fenced code).
    implementation(libs.compose.richtext.commonmark)
    implementation(libs.compose.richtext.ui.material3)
    // UnifiedPush: receive server-pushed briefings/alerts via a distributor (NextPush) — no FCM,
    // no always-on socket. Exclude the connector's JVM tink; security-crypto needs tink-android
    // (Android Keystore), so keep only that and bump it high enough for the connector's classes to
    // resolve — otherwise the two Tink artifacts collide (duplicate classes).
    implementation(libs.unifiedpush.connector) {
        exclude(group = "com.google.crypto.tink", module = "tink")
    }
    implementation(libs.tink.android)
    // grouse core + roam transport: uniffi Kotlin bindings over the locally built
    // cdylibs (arm64-v8a), packaged by the :grouse-core-aar module.
    implementation(project(":grouse-core-aar"))
    // QR pairing for roam hosts: CameraX preview + zxing core (pure Java — the
    // ML Kit barcode engine was ~19 MB of native libbarhopper across 4 ABIs for
    // one QR decode; zxing is ~700 KB with zero natives).
    implementation(libs.camera.camera2)
    implementation(libs.camera.lifecycle)
    implementation(libs.camera.view)
    implementation(libs.zxing.core)

    // JVM unit tests: parsers and wire framing only — no Android framework, no Robolectric.
    // Defends the ACP contracts that have bitten repeatedly (casing, session_info_update keys,
    // extension DTO shapes, _meta.client). See app/src/test/java/id/gauvin/grouse/.
    testImplementation(libs.junit)
    // ChartHtmlTest exercises chartHtml() -> org.json.JSONObject.quote; the android.jar
    // org.json stub is not mocked in local unit tests, so use the real library on the
    // test classpath (it shadows the stub). Test-only; the app uses the framework's own.
    testImplementation(libs.json)
}
