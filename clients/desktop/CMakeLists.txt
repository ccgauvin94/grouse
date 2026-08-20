cmake_minimum_required(VERSION 3.16)

project(grouse-desktop VERSION 0.1.0 LANGUAGES CXX)

include(CTest)   # defines BUILD_TESTING (default ON); tests live in tests/

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(CMAKE_AUTOMOC ON)
set(CMAKE_AUTORCC ON)
set(CMAKE_AUTOUIC ON)

find_package(Qt6 REQUIRED COMPONENTS Quick Qml QuickControls2 Widgets DBus)
find_package(ECM REQUIRED NO_MODULE)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH})
find_package(KF6 REQUIRED COMPONENTS Kirigami CoreAddons)

# KRunner integration plugin (host-side; OFF for the flatpak build).
option(BUILD_KRUNNER "Build the KRunner session-search plugin" ON)

qt_add_executable(grouse-desktop
    src/main.cpp
    src/dbusadapter.h
    src/dbusadapter.cpp
    src/corebridge.h
    src/corebridge.cpp
    src/roamlistmodel.h
    src/roamlistmodel.cpp
    src/manager.h
    src/manager.cpp
    src/messagelistmodel.h
    src/messagelistmodel.cpp
    src/sessionlistmodel.h
    src/sessionlistmodel.cpp
    src/markdown.h
    src/markdown.cpp
)

target_include_directories(grouse-desktop PRIVATE src)

target_link_libraries(grouse-desktop PRIVATE
    Qt6::Quick
    Qt6::Qml
    Qt6::QuickControls2
    Qt6::Widgets
    Qt6::DBus
    KF6::Kirigami
    KF6::CoreAddons
)

qt_add_resources(grouse-desktop "app"
    PREFIX "/"
    BASE "${CMAKE_CURRENT_SOURCE_DIR}/ui"
    FILES
        ui/main.qml
        ui/GrouseTheme.qml
        ui/ChatPage.qml
        ui/ChartBubble.qml
        ui/ConnectDialog.qml
        ui/SettingsDialog.qml
        ui/PermissionDialog.qml
        ui/GlobalExtensionsDialog.qml
        ui/ProvidersDialog.qml
        ui/RecipesDialog.qml
        ui/SchedulerDialog.qml
        ui/SkillsDialog.qml
        ui/ProjectDialog.qml
)

install(TARGETS grouse-desktop
    BUNDLE DESTINATION .
    RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR}
)

install(FILES
    ${CMAKE_CURRENT_SOURCE_DIR}/data/id.gauvin.Grouse.desktop
    DESTINATION ${CMAKE_INSTALL_DATADIR}/applications
)
install(FILES
    ${CMAKE_CURRENT_SOURCE_DIR}/data/hicolor/scalable/apps/id.gauvin.Grouse.svg
    DESTINATION ${CMAKE_INSTALL_DATAROOTDIR}/icons/hicolor/scalable/apps
)

if(BUILD_TESTING)
    add_subdirectory(tests)
endif()

if(BUILD_KRUNNER)
    add_subdirectory(krunner)
endif()
