# JNA and the UniFFI bindings reach these classes through reflection and JNI.
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-keep class app.rustdrop.core.** { *; }
-dontwarn java.awt.**
