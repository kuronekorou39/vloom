import java.util.Properties

plugins {
    id("com.android.application")
    id("kotlin-android")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// リリース署名。android/key.properties があればその鍵で署名する (CI は Secrets から生成)。
// 無ければ debug 鍵にフォールバックし、鍵を持たない人でも `flutter build apk` が通るようにする。
// debug 鍵は全開発者共通かつ CI ではビルドごとに再生成されるため、配布物には使えない。
val keystorePropertiesFile = rootProject.file("key.properties")
val keystoreProperties = Properties().apply {
    if (keystorePropertiesFile.exists()) {
        keystorePropertiesFile.inputStream().use { load(it) }
    }
}
val hasReleaseKey = keystorePropertiesFile.exists()

android {
    namespace = "app.vloom.vloom"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = JavaVersion.VERSION_17.toString()
    }

    defaultConfig {
        applicationId = "app.vloom.vloom"
        // 計測用ビルドを配布版と併存させるための任意サフィックス。
        //   flutter build apk --release -PappIdSuffix=.lab
        // 配布版とは別パッケージになるので、リリース鍵を持たない環境でも
        // 既存インストール (と受信履歴) を消さずに横に入れられる。
        // 未指定なら空文字なので通常のビルドには影響しない。
        val appIdSuffix = (project.findProperty("appIdSuffix") as String?) ?: ""
        applicationIdSuffix = appIdSuffix
        // ランチャー上で見分けが付くようにラベルも変える
        // (マニフェストの android:label がこのプレースホルダを参照する)。
        manifestPlaceholders["appLabel"] =
            if (appIdSuffix.isEmpty()) "Vloom" else "Vloom (${appIdSuffix.removePrefix(".")})"
        minSdk = flutter.minSdkVersion
        targetSdk = flutter.targetSdkVersion
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    signingConfigs {
        if (hasReleaseKey) {
            create("release") {
                storeFile = file(keystoreProperties.getProperty("storeFile"))
                storePassword = keystoreProperties.getProperty("storePassword")
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            signingConfig = if (hasReleaseKey) {
                signingConfigs.getByName("release")
            } else {
                signingConfigs.getByName("debug")
            }
        }
    }
}

flutter {
    source = "../.."
}
