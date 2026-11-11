import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// set by build/android/build.sh (and so by CI): the version of the other builds,
// 0.2.<commits>, and the number of commits as the version code
val magicdabVersion = providers.gradleProperty("magicdabVersion").getOrElse("0.0.0-dev")
val magicdabVersionCode = providers.gradleProperty("magicdabVersionCode").getOrElse("1").toInt()

// release signing from the environment (CI secrets); without it the release APK is unsigned
val keystore = providers.environmentVariable("MAGICDAB_KEYSTORE").orNull?.let { file(it) }?.takeIf { it.exists() }

android {
    namespace = "pl.techkrzysiek.magicdab"
    compileSdk = 36

    defaultConfig {
        applicationId = "pl.techkrzysiek.magicdab"
        // (Android 8: notification channels, adaptive icons, audio focus requests)
        minSdk = 26
        targetSdk = 36
        versionCode = magicdabVersionCode
        versionName = magicdabVersion
    }

    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = keystore
                storePassword = providers.environmentVariable("MAGICDAB_KEYSTORE_PASSWORD").orNull
                keyAlias = providers.environmentVariable("MAGICDAB_KEY_ALIAS").orNull
                keyPassword = providers.environmentVariable("MAGICDAB_KEY_PASSWORD").orNull
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            if (keystore != null) signingConfig = signingConfigs.getByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    packaging {
        jniLibs {
            // the server is a program run from the native library folder: it has
            // to be unpacked there at installation, not mapped from the APK
            useLegacyPackaging = true
            // (stripped by cargo already; no NDK needed here)
            keepDebugSymbols += "**/libmagicdab.so"
        }
    }

    lint {
        // (lint runs on demand: `gradle lint`)
        checkReleaseBuilds = false
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

// an APK without the server would not work: it is built first (build/android/build.sh)
val checkServer by tasks.registering {
    val libs = file("src/main/jniLibs")
    doLast {
        if (libs.walkTopDown().none { it.name == "libmagicdab.so" }) {
            throw GradleException("no libmagicdab.so in $libs: build the server first (build/android/build.sh)")
        }
    }
}
tasks.named("preBuild") { dependsOn(checkServer) }
