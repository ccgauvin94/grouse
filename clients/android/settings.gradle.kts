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
// The old dev.grouse:roamcore maven repo is gone: the native transport now ships as the
// local :grouse-core-aar module (uniffi bindings + cdylibs) inside this repo.
rootProject.name = "grouse-android"
include(":app", ":grouse-core-aar")
