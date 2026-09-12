# R8 rules for the release build.

# uniffi's generated bindings are reached from native code and JNA by name:
# JNA maps each `external fun` to an exported symbol and reflects over the
# @Structure classes. Keep the whole generated package (both crates) and the
# native method names, or the FFI registration fails at class-init on device.
-keep class uniffi.grouse_core.** { *; }
-keep class uniffi.grouse_roam_core.** { *; }
-keepclasseswithmembernames class * {
    native <methods>;
}

# JNA itself reflects over Structure fields and Library interfaces.
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.Structure { *; }
-dontwarn com.sun.jna.**

# Google's Tink (pulled in by security-crypto and UnifiedPush) resolves some
# primitives reflectively.
-keep class com.google.crypto.tink.** { *; }
-dontwarn com.google.crypto.tink.**

# kotlinx.serialization, Compose, AndroidX, OkHttp and CameraX all ship their
# own consumer ProGuard rules, so nothing extra is needed for them here.
