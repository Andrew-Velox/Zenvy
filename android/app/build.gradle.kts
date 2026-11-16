plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "app.zenvy.remote"
    compileSdk = 35

    defaultConfig {
        applicationId = "app.zenvy.remote"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    buildFeatures { buildConfig = true }

    // Release signing comes from the environment, never from a file in the repository: set ZENVY_KEYSTORE (path to your
    // own keystore) and ZENVY_KEYSTORE_PASSWORD (plus ZENVY_KEY_ALIAS, default "zenvy", and ZENVY_KEY_PASSWORD, default
    // the keystore password). Without them `assembleRelease` still builds, but the APK is unsigned and cannot be installed.
    val keystore = System.getenv("ZENVY_KEYSTORE")
    signingConfigs {
        if (!keystore.isNullOrBlank()) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("ZENVY_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("ZENVY_KEY_ALIAS") ?: "zenvy"
                keyPassword = System.getenv("ZENVY_KEY_PASSWORD") ?: System.getenv("ZENVY_KEYSTORE_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            if (!keystore.isNullOrBlank()) signingConfig = signingConfigs.getByName("release")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
}

dependencies {
    // Google's code scanner: no CAMERA permission needed, no camera code in this app.
    implementation("com.google.android.gms:play-services-code-scanner:16.1.0")
    testImplementation("junit:junit:4.13.2")
}
