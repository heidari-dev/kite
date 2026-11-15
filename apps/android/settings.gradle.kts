pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "kite"
include(":app")

// The Android Gradle Plugin pulls in older versions of these for the build itself (not the app),
// and they have advisories. Every configuration, the plugin classpath included, gets the patched
// version. shortcut: this forces the version even if something asks for a newer one; drop an entry
// once the plugin ships a fixed version itself.
val patched = mapOf(
    "com.google.guava:guava" to "33.8.0-jre",
    "org.apache.commons:commons-lang3" to "3.22.0",
    "org.apache.httpcomponents:httpclient" to "4.5.14",
    "org.bitbucket.b_c:jose4j" to "0.9.7",
    "org.bouncycastle:bcprov-jdk18on" to "1.85",
    "org.bouncycastle:bcpkix-jdk18on" to "1.85",
    "org.bouncycastle:bcutil-jdk18on" to "1.85",
    "org.jdom:jdom2" to "2.0.6.1",
    "org.jetbrains.kotlin:kotlin-gradle-plugin" to "2.4.21",
)

gradle.beforeProject {
    val fix = Action<DependencyResolveDetails> {
        patched["${requested.group}:${requested.name}"]?.let {
            useVersion(it)
            because("known advisories in older versions")
        }
    }
    buildscript.configurations.configureEach { resolutionStrategy.eachDependency(fix) }
    configurations.configureEach { resolutionStrategy.eachDependency(fix) }
}
