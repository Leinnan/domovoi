plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

val domovoiAbi = providers.gradleProperty("domovoiAbi").orNull?.takeUnless { it == "all" }
require(domovoiAbi == null || domovoiAbi in setOf("arm64-v8a", "x86_64")) {
    "domovoiAbi must be all, arm64-v8a, or x86_64"
}

android {
    namespace = "com.crayen.domovoi"
    compileSdk = 37
    ndkVersion = "29.0.13846066"

    defaultConfig {
        applicationId = "com.crayen.domovoi"
        minSdk = 29
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        if (domovoiAbi != null) ndk.abiFilters.add(domovoiAbi)
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug")
        }
    }
    buildFeatures {
        compose = true
        buildConfig = true
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    packaging {
        // The core's debug symbols stay out of the APK.
        jniLibs.keepDebugSymbols.clear()
    }
    testOptions {
        unitTests.isReturnDefaultValues = true
    }
}

kotlin {
    jvmToolchain(17)
    compilerOptions {
        optIn.add("kotlin.ExperimentalUnsignedTypes")
        optIn.add("androidx.compose.material3.ExperimentalMaterial3Api")
        optIn.add("androidx.compose.material3.adaptive.ExperimentalMaterial3AdaptiveApi")
        optIn.add("androidx.compose.foundation.layout.ExperimentalLayoutApi")
    }
}

dependencies {
    implementation(project(":shared"))

    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.process)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.coroutines.android)

    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.compose.material3)
    implementation(libs.compose.material.icons)
    implementation(libs.compose.adaptive)
    implementation(libs.compose.adaptive.layout)
    implementation(libs.compose.adaptive.navigation3)
    implementation(libs.compose.navigation.suite)
    implementation(libs.navigation3.runtime)
    implementation(libs.navigation3.ui)
    implementation(libs.coil.compose)
    implementation(libs.coil.okhttp)
    debugImplementation(libs.compose.ui.tooling)

    implementation(libs.okhttp)
    implementation(libs.camera.camera2)
    implementation(libs.camera.lifecycle)
    implementation(libs.camera.view)
    implementation(libs.mlkit.barcode)
    implementation(libs.zxing.core)
    implementation(libs.glance.appwidget)
    implementation(libs.glance.material3)

    testImplementation(libs.junit)
    testImplementation(libs.coroutines.test)
    testImplementation(libs.okhttp.mockwebserver)
    testImplementation(libs.org.json)
}
