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
)

include_directories(
    ${CMAKE_CURRENT_SOURCE_DIR}
)

add_library(NerveDeskDisplay SHARED
    Driver.h
    Driver.cpp
    Edid.h
)

target_link_libraries(NerveDeskDisplay PRIVATE
    WdfLdr
    WdfDriverSupport
    dxgi
    d3d11
)

set_target_properties(NerveDeskDisplay PROPERTIES
    PREFIX ""
    OUTPUT_NAME "NerveDeskDisplay"
    SUFFIX ".dll"
)
