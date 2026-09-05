# The JNI bridge is reached from Rust by name; R8 must not rename or remove it.
-keep class io.tempest.android.core.TempestBridge { *; }
# The callback object Rust invokes by name (onEvent, secretGet, secretSet, ...).
-keep class io.tempest.android.core.TempestBridge$* { *; }
-keepclasseswithmembernames class * {
    native <methods>;
}

# kotlinx.serialization generates serializers reflectively at the boundary.
-keepattributes *Annotation*, InnerClasses
-dontnote kotlinx.serialization.**
-keepclassmembers class io.tempest.android.core.** {
    *** Companion;
}
-keepclasseswithmembers class io.tempest.android.core.** {
    kotlinx.serialization.KSerializer serializer(...);
}
