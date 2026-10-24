# Same as vcpkg's x64-windows-static-md, but builds the release configuration
# only.
#
# vcpkg builds debug and release by default. The project links the release
# static libraries only, so the debug build doubles the time for no benefit.

set(VCPKG_TARGET_ARCHITECTURE x64)
# Dynamic CRT, because that is what Rust uses. A static CRT fails to link;
# `wwxds-media/build.rs` rejects a `+crt-static` build for the same reason.
set(VCPKG_CRT_LINKAGE dynamic)
set(VCPKG_LIBRARY_LINKAGE static)
set(VCPKG_BUILD_TYPE release)
