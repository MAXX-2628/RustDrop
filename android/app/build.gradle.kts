plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

// The Rust core (../../rust-core) is built for Android and its Kotlin bindings generated on every build.
val repo: File = rootDir.parentFile
val jniOut = layout.buildDirectory.dir("generated/jniLibs")
val bindingsOut = layout.buildDirectory.dir("generated/uniffi")
val abis = listOf("arm64-v8a", "armeabi-v7a", "x86_64")

android {
    namespace = "app.rustdrop"
    compileSdk = 37

    defaultConfig {
        applicationId = "app.rustdrop"
        minSdk = 29 // MediaStore.Downloads with RELATIVE_PATH, no storage permission needed
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += abis }
    }

    buildFeatures { compose = true }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    sourceSets["main"].apply {
        kotlin.directories += bindingsOut.get().asFile.path
        jniLibs.directories += jniOut.get().asFile.path
        assets.directories += File(repo, "shared").path // design tokens: assets/design/*.json
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }
}

val cargoNdk = tasks.register<Exec>("cargoNdk") {
    workingDir = repo
    commandLine(
        listOf("cargo", "ndk") + abis.flatMap { listOf("-t", it) } +
            listOf("-o", jniOut.get().asFile.path, "build", "--release", "-p", "rustdrop-core", "--features", "uniffi", "--lib"),
    )
}

val uniffiBindings = tasks.register<Exec>("uniffiBindings") {
    dependsOn(cargoNdk)
    workingDir = repo
    commandLine(
        "cargo", "run", "-q", "-p", "uniffi-bindgen", "--", "generate",
        "--library", File(repo, "target/aarch64-linux-android/release/librustdrop_core.so").path,
        "--language", "kotlin", "--out-dir", bindingsOut.get().asFile.path, "--no-format",
    )
}

tasks.named("preBuild") { dependsOn(uniffiBindings) }

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2026.09.00")
    implementation(composeBom)
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.core:core-ktx:1.19.1")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")
    implementation("androidx.documentfile:documentfile:1.1.0")
    implementation("com.composables:icons-lucide:1.1.0")
    implementation("net.java.dev.jna:jna:5.19.1@aar") // required by the UniFFI Kotlin bindings
    implementation("com.journeyapps:zxing-android-embedded:4.3.0") // QR pairing scanner
    implementation("com.google.zxing:core:3.5.4")
}
