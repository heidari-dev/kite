// Newer versions of what the Android Gradle Plugin pulls in for the build itself (not the app),
// where the versions it ships have known advisories. Constraints only raise a version.
buildscript {
    repositories {
        google()
        mavenCentral()
    }
    dependencies {
        constraints {
            for (notation in listOf(
                "com.google.guava:guava:33.8.0-jre",
                "org.apache.commons:commons-lang3:3.22.0",
                "org.apache.httpcomponents:httpclient:4.5.14",
                "org.bitbucket.b_c:jose4j:0.9.7",
                "org.bouncycastle:bcprov-jdk18on:1.85",
                "org.bouncycastle:bcpkix-jdk18on:1.85",
                "org.bouncycastle:bcutil-jdk18on:1.85",
                "org.jdom:jdom2:2.0.6.1",
                "org.jetbrains.kotlin:kotlin-gradle-plugin:2.4.21",
            )) {
                classpath(notation)
            }
        }
    }
}

plugins {
    id("com.android.application") version "9.5.1" apply false
}
