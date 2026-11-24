cmake_minimum_required(VERSION 3.16)

project(grouse-desktop VERSION 0.1.0 LANGUAGES CXX)

include(CTest)   # defines BUILD_TESTING (default ON); tests live in tests/

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(CMAKE_AUTOMOC ON)
set(CMAKE_AUTORCC ON)
set(CMAKE_AUTOUIC ON)

find_package(Qt6 REQUIRED COMPONENTS Quick Qml QuickControls2 Widgets DBus Network)
find_package(ECM REQUIRED NO_MODULE)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH})
find_package(KF6 REQUIRED COMPONENTS Kirigami CoreAddons)

# Inline MCP-App rendering needs QtWebEngine — absent from the bare KDE
# flatpak runtime, provided by the io.qt.qtwebengine.BaseApp base layer (and
# by distrobox/Fedora via qt6-qtwebengine). Optional: when missing, the app
# still serves MCP Apps through the loopback bridge in the system browser.
find_package(Qt6 QUIET COMPONENTS WebEngineQuick)

# KRunner integration plugin (host-side; OFF for the flatpak build).
option(BUILD_KRUNNER "Build the KRunner session-search plugin" ON)

# The app's core: every source except main.cpp. The test library consumes the
# same list, so a file added here cannot silently disappear from the tests (or
# vice versa). Keep it absolute: tests live in a subdirectory.
set(GROUSE_CORE_SOURCES
    ${CMAKE_CURRENT_SOURCE_DIR}/src/catalogcontroller.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/catalogcontroller.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/chatcontroller.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/chatcontroller.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/coreapi.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/coreapi.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/settingsstore.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/settingsstore.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/transcriptprojection.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/transcriptprojection.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/dbusadapter.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/dbusadapter.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/appbridge.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/appbridge.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/corebridge.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/corebridge.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/roamlistmodel.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/roamlistmodel.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/manager.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/manager.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/messagelistmodel.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/messagelistmodel.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/sessionlistmodel.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/sessionlistmodel.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/markdown.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/markdown.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/myday.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/myday.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/notifier.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/notifier.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/src/pushclient.h
    ${CMAKE_CURRENT_SOURCE_DIR}/src/pushclient.cpp
)

qt_add_executable(grouse-desktop
    ${CMAKE_CURRENT_SOURCE_DIR}/src/main.cpp
    ${GROUSE_CORE_SOURCES}
)

target_include_directories(grouse-desktop PRIVATE src)

target_link_libraries(grouse-desktop PRIVATE
    Qt6::Quick
    Qt6::Qml
    Qt6::QuickControls2
    Qt6::Widgets
    Qt6::DBus
    Qt6::Network
    KF6::Kirigami
    KF6::CoreAddons
)

# Inline MCP-App rendering: WebEngine comes from the flatpak base app (or the
# system package when built natively). Absent -> browser-handoff fallback only.
if(Qt6WebEngineQuick_FOUND)
    target_link_libraries(grouse-desktop PRIVATE Qt6::WebEngineQuick)
    target_compile_definitions(grouse-desktop PRIVATE GROUSE_WEBENGINE=1)
    message(STATUS "grouse-desktop: inline MCP-App rendering ENABLED (QtWebEngine)")
else()
    message(STATUS "grouse-desktop: QtWebEngine not found — MCP Apps open in the system browser")
endif()

qt_add_resources(grouse-desktop "app"
    PREFIX "/"
    BASE "${CMAKE_CURRENT_SOURCE_DIR}/ui"
    FILES
        ui/main.qml
        ui/GrouseTheme.qml
        ui/ChatPage.qml
        ui/MyDayPage.qml
        ui/ChatSidePanel.qml
        ui/ContextBreakdown.qml
        ui/ContextRing.qml
        ui/icons/dev-mode-symbolic.svg
        ui/icons/chat-symbolic.svg
        ui/ChartBubble.qml
        ui/AttachmentCard.qml
        ui/FileCard.qml
        ui/ImageCard.qml
        ui/AppView.qml
        ui/ConnectDialog.qml
        ui/SettingsDialog.qml
        ui/PermissionDialog.qml
        ui/FormDialog.qml
        ui/TurnLogDialog.qml
        ui/GlobalExtensionsDialog.qml
        ui/GooseConfigDialog.qml
        ui/RecipesDialog.qml
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
# Lets the session bus wake us for a push while the window is closed (flatpak exports
# this from the app image to the host). See docs/NOTIFICATIONS.md.
install(FILES
    ${CMAKE_CURRENT_SOURCE_DIR}/data/id.gauvin.Grouse.service
    DESTINATION ${CMAKE_INSTALL_DATADIR}/dbus-1/services
)

if(BUILD_TESTING)
    # tests/CMakeLists consumes ${GROUSE_CORE_SOURCES}: a new source cannot be
    # added to the app without also being compiled into every test target.
    add_subdirectory(tests)
endif()

if(BUILD_KRUNNER)
    add_subdirectory(krunner)
endif()
