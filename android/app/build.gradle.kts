plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
}

android {
    namespace = "io.tempest.android"
    compileSdk = 35

    defaultConfig {
        applicationId = "io.tempest.android"

        // Android 10 is the floor, and the reason is technical rather than
        // arbitrary. API 29 is where W^X arrived: an app may no longer
        // execve() files in its own data directory, only files in
        // nativeLibraryDir. The whole runtime design — shipping PRoot in the
        // APK and letting it map guest binaries — exists to satisfy that rule.
        // Supporting older releases would mean a second, untested execution
        // path. In practice the compatibility stack also needs a 64-bit ARM
        // device with a Vulkan 1.1 driver, which is Android 10 era anyway.
        minSdk = 29
        targetSdk = 35

        versionCode = 2
        versionName = "0.2.0"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

        ndk {
            // arm64 only: the Windows compatibility stack (ARM64 Wine plus the
            // FEX/Box64 emulators) is built for aarch64 and nothing else.
            // Shipping armeabi-v7a or x86 would produce an app that installs
            // and then cannot run a single game.
            abiFilters += "arm64-v8a"
        }
    }

    // Debug builds use AGP's auto-generated debug keystore, so CI produces an
    // installable APK with no secret configured anywhere and no key committed.

    buildTypes {
        debug {
            isMinifyEnabled = false
            applicationIdSuffix = ".debug"
            versionNameSuffix = "-debug"
        }
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
            // Left unsigned. The project must build with no secret configured
            // anywhere, so no signing config is wired up.
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
        isCoreLibraryDesugaringEnabled = false
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    packaging {
        // The native binaries Tempest executes (PRoot and its loaders) must be
        // extracted to nativeLibraryDir at install time. Left compressed inside
        // the APK they would only be mappable, not executable, which is exactly
        // the restriction this design works within.
        jniLibs {
            useLegacyPackaging = true
        }
        resources {
            excludes += setOf(
                "/META-INF/{AL2.0,LGPL2.1}",
                "/META-INF/DEPENDENCIES",
            )
        }
    }

    sourceSets {
        getByName("main") {
            // Populated by the Rust and PRoot build steps.
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }

    testOptions {
        unitTests {
            isIncludeAndroidResources = true
            isReturnDefaultValues = true
        }
    }

    lint {
        // A lint regression should fail the build rather than scroll past in a
        // CI log, but missing translations are not a correctness problem.
        warningsAsErrors = false
        abortOnError = true
        disable += setOf("MissingTranslation", "UnusedResources")
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.ktx)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.activity.compose)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.json)

    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.ui)
    implementation(libs.androidx.ui.graphics)
    implementation(libs.androidx.ui.tooling.preview)
    implementation(libs.androidx.material3)
    implementation(libs.androidx.material.icons.extended)
    implementation(libs.coil.compose)

    debugImplementation(libs.androidx.ui.tooling)
    debugImplementation(libs.androidx.ui.test.manifest)

    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)

    androidTestImplementation(libs.androidx.junit)
    androidTestImplementation(libs.androidx.espresso.core)
    androidTestImplementation(platform(libs.androidx.compose.bom))
    androidTestImplementation(libs.androidx.ui.test.junit4)
}
