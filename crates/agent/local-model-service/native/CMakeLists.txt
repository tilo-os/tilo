cmake_minimum_required(VERSION 3.16)
project(tilo_speech_cpu LANGUAGES C CXX)

# An image built on a newer runner must still run on Tilo's CPU baseline.
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
# ggml otherwise wraps every compile in whatever ccache or sccache is on PATH.
# A shared sccache server may not see this build directory or the compiler,
# so the first object fails to spawn. Cargo already decides on caching.
set(GGML_CCACHE OFF CACHE BOOL "" FORCE)
set(GGML_BACKEND_DL OFF CACHE BOOL "" FORCE)
set(GGML_NATIVE OFF CACHE BOOL "" FORCE)
set(GGML_AVX512 OFF CACHE BOOL "" FORCE)
set(GGML_AVX512_VBMI OFF CACHE BOOL "" FORCE)
set(GGML_AVX512_VNNI OFF CACHE BOOL "" FORCE)
set(GGML_AVX512_BF16 OFF CACHE BOOL "" FORCE)
set(GGML_AMX_TILE OFF CACHE BOOL "" FORCE)
set(GGML_AMX_INT8 OFF CACHE BOOL "" FORCE)
set(GGML_AMX_BF16 OFF CACHE BOOL "" FORCE)
set(GGML_OPENMP OFF CACHE BOOL "" FORCE)
set(GGML_CPU_KLEIDIAI OFF CACHE BOOL "" FORCE)
set(GGML_CUDA OFF CACHE BOOL "" FORCE)
set(GGML_VULKAN OFF CACHE BOOL "" FORCE)
set(GGML_METAL OFF CACHE BOOL "" FORCE)
set(GGML_BLAS OFF CACHE BOOL "" FORCE)
set(GGML_RPC OFF CACHE BOOL "" FORCE)
set(WHISPER_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(WHISPER_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(WHISPER_BUILD_SERVER OFF CACHE BOOL "" FORCE)
set(WHISPER_CURL OFF CACHE BOOL "" FORCE)
set(WHISPER_COREML OFF CACHE BOOL "" FORCE)
set(WHISPER_OPENVINO OFF CACHE BOOL "" FORCE)
set(CMAKE_INSTALL_LIBDIR lib CACHE STRING "" FORCE)

if(CMAKE_SYSTEM_PROCESSOR MATCHES "x86_64|AMD64|amd64")
    # Match the repository's x86-64-v3 OS floor, not the build runner.
    set(GGML_SSE42 ON CACHE BOOL "" FORCE)
    set(GGML_AVX ON CACHE BOOL "" FORCE)
    set(GGML_AVX2 ON CACHE BOOL "" FORCE)
    set(GGML_FMA ON CACHE BOOL "" FORCE)
    set(GGML_F16C ON CACHE BOOL "" FORCE)
    set(GGML_BMI2 ON CACHE BOOL "" FORCE)
endif()
if(CMAKE_SYSTEM_PROCESSOR MATCHES "aarch64|arm64|ARM64")
    set(GGML_CPU_ARM_ARCH armv8-a CACHE STRING "" FORCE)
endif()
# Fedora's ar/ranlib defaults retain archive timestamps. Those bytes become
# Rust static-library cache inputs even when every native object is unchanged.
# Use deterministic archives for both languages, including the symbol index.
foreach(language C CXX)
    set(CMAKE_${language}_ARCHIVE_CREATE
        "<CMAKE_AR> qcD <TARGET> <LINK_FLAGS> <OBJECTS>")
    set(CMAKE_${language}_ARCHIVE_APPEND
        "<CMAKE_AR> qD <TARGET> <LINK_FLAGS> <OBJECTS>")
    set(CMAKE_${language}_ARCHIVE_FINISH "<CMAKE_RANLIB> -D <TARGET>")
endforeach()
# This is a vendored archive subtree, not its own Git checkout. Upstream's
# parent-HEAD probe otherwise embeds Tilo's changing commit in every ggml-base
# compile command, invalidating the compiler cache for unchanged CPU sources.
# Report the actual vendored source identity and leave Tilo's build provenance
# to the enclosing package/image receipts.
file(GLOB_RECURSE TILO_WHISPER_INPUTS CONFIGURE_DEPENDS
    RELATIVE "${TILO_WHISPER_ROOT}" "${TILO_WHISPER_ROOT}/*")
list(SORT TILO_WHISPER_INPUTS)
set(TILO_WHISPER_DIGESTS "")
foreach(input IN LISTS TILO_WHISPER_INPUTS)
    file(SHA256 "${TILO_WHISPER_ROOT}/${input}" digest)
    string(APPEND TILO_WHISPER_DIGESTS "${input}:${digest}\n")
endforeach()
string(SHA256 TILO_WHISPER_SOURCE_SHA256 "${TILO_WHISPER_DIGESTS}")
set(GIT_EXE FALSE)
set(GGML_BUILD_COMMIT "sha256:${TILO_WHISPER_SOURCE_SHA256}")
add_subdirectory("${TILO_WHISPER_ROOT}" whisper)
