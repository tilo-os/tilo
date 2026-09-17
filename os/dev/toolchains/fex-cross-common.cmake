# Used only by FEX's guest-library and guest-test sub-builds. The generator and
# host libraries continue to use the native ARM compiler and native libraries.
set(CMAKE_SYSTEM_NAME Linux)
if(NOT X86_DEV_ROOTFS)
  set(X86_DEV_ROOTFS "$ENV{TILO_FEX_X86_SYSROOT}" CACHE PATH "Complete x86 SDK")
endif()
if(NOT IS_ABSOLUTE "${X86_DEV_ROOTFS}" OR X86_DEV_ROOTFS STREQUAL "/"
   OR NOT IS_DIRECTORY "${X86_DEV_ROOTFS}/usr/include")
  message(FATAL_ERROR "A separate complete x86 SDK is required")
endif()
set(CMAKE_C_COMPILER /usr/bin/clang)
set(CMAKE_CXX_COMPILER /usr/bin/clang++)
set(CMAKE_SYSROOT "${X86_DEV_ROOTFS}")
set(CMAKE_C_COMPILER_EXTERNAL_TOOLCHAIN "${X86_DEV_ROOTFS}/usr")
set(CMAKE_CXX_COMPILER_EXTERNAL_TOOLCHAIN "${X86_DEV_ROOTFS}/usr")
set(CMAKE_EXE_LINKER_FLAGS_INIT "-fuse-ld=lld")
set(CMAKE_MODULE_LINKER_FLAGS_INIT "-fuse-ld=lld")
set(CMAKE_SHARED_LINKER_FLAGS_INIT "-fuse-ld=lld")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
set(ENV{PKG_CONFIG_SYSROOT_DIR} "${X86_DEV_ROOTFS}")
# Fedora uses lib64 for x86-64 and lib for i686. Also recognize the standard
# GNU triplet layout without falling back to host pkg-config directories.
set(ENV{PKG_CONFIG_LIBDIR} "${X86_DEV_ROOTFS}/usr/${TILO_FEX_LIBDIR}/pkgconfig:${X86_DEV_ROOTFS}/usr/lib/${CMAKE_C_COMPILER_TARGET}/pkgconfig:${X86_DEV_ROOTFS}/usr/share/pkgconfig")
set(ENV{PKG_CONFIG_PATH} "")
list(APPEND CMAKE_TRY_COMPILE_PLATFORM_VARIABLES X86_DEV_ROOTFS)
