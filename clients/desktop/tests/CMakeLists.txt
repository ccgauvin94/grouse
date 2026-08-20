# Test suite for grouse-desktop. Built only when BUILD_TESTING is ON (default
# for dev builds; the flatpak manifest passes -DBUILD_TESTING=OFF).
#
# Layout:
#   tst_markdown / tst_messagelistmodel / tst_sessionlistmodel — pure unit tests
#   tst_manager    — thin-client intent mapping: Manager Q_INVOKABLE -> CoreBridge ABI
#   tst_roamlistmodel — roam peer-list model tests
#   tst_qml        — QtQuickTest run of tests/qml/tst_*.qml (ChartBubble)
#
# Run everything with `ctest --test-dir build` (or `ctest -R <name> --verbose`).

find_package(Qt6 REQUIRED COMPONENTS Test QuickTest)

# The app's core minus main.cpp, compiled once and shared by every C++ test.
# libgrouse_core.so is dlopen'd at runtime (never linked) — see src/corebridge.
add_library(grouse_core STATIC
    ${CMAKE_SOURCE_DIR}/src/corebridge.cpp
    ${CMAKE_SOURCE_DIR}/src/manager.cpp
    ${CMAKE_SOURCE_DIR}/src/messagelistmodel.cpp
    ${CMAKE_SOURCE_DIR}/src/sessionlistmodel.cpp
    ${CMAKE_SOURCE_DIR}/src/markdown.cpp
    ${CMAKE_SOURCE_DIR}/src/roamlistmodel.cpp
)
target_include_directories(grouse_core PUBLIC ${CMAKE_SOURCE_DIR}/src)
target_link_libraries(grouse_core PUBLIC Qt6::Widgets)

foreach(test_name tst_markdown tst_messagelistmodel tst_sessionlistmodel
                  tst_manager tst_roamlistmodel)
    qt_add_executable(${test_name} ${test_name}.cpp)
    target_link_libraries(${test_name} PRIVATE grouse_core Qt6::Test)
    add_test(NAME ${test_name} COMMAND ${test_name})
endforeach()

# QML component tests: every tests/qml/tst_*.qml is picked up by the runner.
# QuickTest cannot resolve relative imports against the test file, so the import
# target (ui/ChartBubble.qml) is baked in as an absolute file: URL at configure
# time; the generated test lives in the build dir, keeping machine-specific
# paths out of the repo.
set(CHART_UI_URL "file://${CMAKE_CURRENT_SOURCE_DIR}/../ui")
configure_file(qml/tst_chartbubble.qml.in
    ${CMAKE_CURRENT_BINARY_DIR}/qml/tst_chartbubble.qml @ONLY)
# Plain QML tests pass through unchanged (no placeholders).
configure_file(qml/tst_combobox.qml
    ${CMAKE_CURRENT_BINARY_DIR}/qml/tst_combobox.qml COPYONLY)

qt_add_executable(tst_qml qml/tst_qml.cpp)
target_link_libraries(tst_qml PRIVATE Qt6::QuickTest Qt6::Test)
# QUICK_TEST_SOURCE_DIR tells the runner where the tst_*.qml files live.
target_compile_definitions(tst_qml PRIVATE QUICK_TEST_SOURCE_DIR="${CMAKE_CURRENT_BINARY_DIR}/qml")
add_test(NAME tst_qml COMMAND tst_qml)
set_tests_properties(tst_qml PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
