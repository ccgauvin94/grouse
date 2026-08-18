# KRunner plugin: searches sessions via the app's session-bus service. This is
# a HOST component (lives outside the flatpak sandbox); the flatpak manifest
# passes -DBUILD_KRUNNER=OFF.
#
# This file is both included from the top-level CMakeLists and buildable
# standalone (build-krunner.sh configures it directly) — keep the
# find_package/setup calls self-sufficient.
cmake_minimum_required(VERSION 3.16)
project(grouserunner LANGUAGES CXX)

# Standalone builds (the top level has already found these; re-finding is a
# no-op then) — must precede include(KDECMakeSettings), which ships with ECM.
find_package(ECM REQUIRED NO_MODULE)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH})

# Runners must be shared plugins: kcoreaddons_add_plugin builds static when
# BUILD_SHARED_LIBS is unset (default OFF), and KPluginMetaData cannot
# discover a static archive.
set(BUILD_SHARED_LIBS ON)
# KDECMakeSettings' RPATH logic requires this up front; default to "lib" so
# plugins always land in ${prefix}/lib/qt6/plugins (Qt6's scan path).
if(NOT CMAKE_INSTALL_LIBDIR)
    set(CMAKE_INSTALL_LIBDIR "lib")
endif()
set(KDE_INSTALL_LIBDIR "${CMAKE_INSTALL_LIBDIR}")
# Qt6 scans ${prefix}/lib/qt6/plugins for plugins; the KDE default omits the
# qt6 segment, which would strand the plugin where nothing looks for it.
set(KDE_INSTALL_PLUGINDIR "${KDE_INSTALL_LIBDIR}/qt6/plugins")
# kcoreaddons_add_plugin requires the KDE install variables and an explicit
# library output directory.
include(KDECMakeSettings)
include(KDEInstallDirs)
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/krunner)

set(CMAKE_AUTOMOC ON)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(Qt6 REQUIRED COMPONENTS DBus Core)
find_package(KF6 REQUIRED COMPONENTS Runner Service I18n CoreAddons)
# kcoreaddons_add_plugin embeds the KPlugin JSON so KRunner can discover the
# plugin by its real id (qt_add_plugin would leave pluginId as the .so name);
# the macro arrives with the KF6 CoreAddons find_package above.

kcoreaddons_add_plugin(grouserunner
    INSTALL_NAMESPACE "kf6/krunner"
    SOURCES
        grouserunner.cpp
        grouserunner.h
)
target_link_libraries(grouserunner PRIVATE
    KF6::Runner
    KF6::Service
    KF6::I18n
    KF6::CoreAddons
    Qt6::DBus
    Qt6::Core
)
