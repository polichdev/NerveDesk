cmake_minimum_required(VERSION 3.20)
project(NerveDeskDisplay LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

add_definitions(
    -DUNICODE
    -D_UNICODE
    -D_WIN32_WINNT=0x0A00
    -DWINVER=0x0A00
    -DUMDF_USING_NTSTATUS
    -DUMDF_DRIVER
    -DIDDCX_VERSION_MAJOR=1
    -DIDDCX_VERSION_MINOR=2
    -DIDDCX_MINIMUM_VERSION_REQUIRED=2
)

include_directories(
    ${CMAKE_CURRENT_SOURCE_DIR}
)

add_library(NerveDeskDisplay SHARED
    Driver.h
    Driver.cpp
)

target_link_libraries(NerveDeskDisplay PRIVATE
    WdfLdr
    WdfDriverSupport
    dxgi
    d3d11
    ws2_32
    OneCoreUAP
)

set_target_properties(NerveDeskDisplay PROPERTIES
    PREFIX ""
    OUTPUT_NAME "NerveDeskDisplay"
    SUFFIX ".dll"
)
