# leveldb-sys provenance

- Package: `leveldb-sys` 2.0.9, exact crates.io archive, 203 regular files.
- Registry checksum: `9dd94a4d0242a437e5e41a27c782b69a624469ca1c4d1e5cb3c337f74a8031d4`.
- Source: <https://static.crates.io/crates/leveldb-sys/leveldb-sys-2.0.9.crate>.
- Binding repository: <https://github.com/skade/leveldb-sys>.
- Recorded upstream revision: `8c051f8ea4574679f076f16ddb45d561c60708ab`.
- Clean archive file-tree SHA-256: `3cc9734a334365a6e5072d8648a59f1a09a76bc82055d562b327cd8002a44c59` (sorted relative paths, each path + NUL + binary SHA-256 of its contents).
- License: MIT binding, BSD LevelDB 1.22 and Snappy 1.1.7; all original license and author files retained.
- Binding `LICENSE.md` SHA-256: `66f2b7613b27ada971353e89021aee6f50fa236124aa10dea374670741b62c91`.
- LevelDB `LICENSE` SHA-256: `ccc19f1da0798ed666609b65a5b44dd8b3abe6fc08b9c0592eb76e82e174db19`.
- Snappy `COPYING` SHA-256: `55172044f7e241207117448a4d9d6ba1d0925c8ad66b5d4c08c70adfa9cc3de6`.

Four local build compatibility patches keep ordinary Cargo builds working with Fedora GCC and CMake 4:

1. `deps/leveldb-1.22/CMakeLists.txt` backports Google's current `CheckCXXCompilerFlag` probe for `-Wthread-safety` from <https://github.com/google/leveldb/blob/main/CMakeLists.txt>. The old source probe passed a semicolon list through `CMAKE_REQUIRED_FLAGS`, which CMake documents as requiring a space-separated string (<https://cmake.org/cmake/help/latest/module/CheckCXXSourceCompiles.html>). The observed GCC build incorrectly cached `HAVE_CLANG_THREAD_SAFETY=1` and failed compiling `db/builder.cc` with the unsupported Clang flag. The new probe preserves the analysis where supported instead of stripping flags or selecting a different compiler.
2. `src/build.rs` gives Snappy and LevelDB separate CMake output/install directories under Cargo's own `OUT_DIR`. This prevents two native projects from sharing one CMake cache while keeping both libraries bundled, statically linked and independently located through their returned install prefixes.
3. `deps/snappy-1.1.7/CMakeLists.txt` raises the policy minimum from 3.1 to 3.5, the compatibility floor supported by CMake 4. No global CMake environment override is needed.

4. `src/lib.rs` spells the existing default C ABI explicitly on the five callback function types (`extern "C" fn`). This removes Rust missing-ABI warnings without changing callback safety, argument types, or ABI.

The C API semantics, engine sources, checksummed WAL recovery, and compression implementation are unchanged. No ambient LevelDB installation is required. The workspace root selects this copy with a reviewed Cargo patch; root manifest and lock updates are separate from the vendor payload.
