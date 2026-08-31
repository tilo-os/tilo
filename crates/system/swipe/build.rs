//! Builds the C-ABI shim (`csrc/swipe_ffi.cpp`) and links it against the
//! prebuilt FUTO `libswipe_decoder.a` + `ExecuTorch` static libraries.
//!
//! The FUTO swipe-library is built out-of-tree via its own `make host`
//! (clones `ExecuTorch` as a submodule). Point this build at that tree with:
//!
//!     QOS_SWIPE_LIB=/path/to/swipe-library   # contains build/ + third_party/executorch/cmake-out-host/ + include/
//!
//! When the env var is unset or the libs are missing, the crate compiles
//! WITHOUT FUTO support (`cfg(futo)` off) so downstream code (the OSK) can
//! fall back to its geometric decoder. This keeps `cargo build` working on
//! machines that haven't built the FUTO stack.

use std::path::PathBuf;

fn main() {
    // (lib name, dir, whole-archive?) — hoisted here so it's declared
    // before any statement in this block, not just before its first use.
    type Lib<'a> = (&'a str, PathBuf, bool);

    println!("cargo:rerun-if-env-changed=QOS_SWIPE_LIB");
    println!("cargo:rerun-if-changed=csrc/swipe_ffi.cpp");
    // Declare the custom cfg so rustc's unexpected_cfgs lint stays quiet.
    println!("cargo:rustc-check-cfg=cfg(futo)");

    let Some(root) = std::env::var_os("QOS_SWIPE_LIB").map(PathBuf::from) else {
        eprintln!("qos-swipe: QOS_SWIPE_LIB unset — building stub (FUTO swipe disabled)");
        return;
    };
    let build_dir = root.join("build");
    let et = root.join("third_party/executorch/cmake-out-host");
    let swipe_lib = build_dir.join("libswipe_decoder.a");
    if !swipe_lib.exists() || !et.join("libexecutorch.a").exists() {
        eprintln!(
            "qos-swipe: libs not found under {} — building stub (FUTO swipe disabled)",
            root.display()
        );
        return;
    }

    // --- compile the C-ABI shim against the FUTO + ExecuTorch headers ---
    let etr = root.join("third_party/executorch");
    let mut cc = cc::Build::new();
    cc.cpp(true)
        .std("c++17")
        .file("csrc/swipe_ffi.cpp")
        .include(root.join("include"))
        .include(&etr) // so `#include "executorch/..."` resolves (root is third_party/executorch/..)
        .include(root.join("third_party"))
        .include(etr.join("runtime/core/portable_type/c10"))
        .include(etr.join("backends/xnnpack/third-party/cpuinfo/include"))
        .include(etr.join("backends/xnnpack/third-party/pthreadpool/include"))
        .warnings(false);
    cc.compile("qos_swipe_shim");

    // --- link the prebuilt static libraries ---
    // We use cargo:rustc-link-lib / link-search (NOT rustc-link-arg) because
    // only these PROPAGATE to dependent crates (the OSK binary). rust-lld
    // resolves the circular references between the ExecuTorch archives
    // without needing an explicit --start-group.
    //
    // Whole-archive libs register ops/backends via static initializers and
    // must not be dead-stripped.
    let libs: &[Lib] = &[
        // whole-archive (registration)
        ("executorch", et.clone(), true),
        ("executorch_selected_kernels", et.clone(), true),
        ("xnnpack_backend", et.join("backends/xnnpack"), true),
        // regular
        (
            "extension_module_static",
            et.join("extension/module"),
            false,
        ),
        ("extension_tensor", et.join("extension/tensor"), false),
        (
            "extension_data_loader",
            et.join("extension/data_loader"),
            false,
        ),
        (
            "extension_flat_tensor",
            et.join("extension/flat_tensor"),
            false,
        ),
        (
            "extension_named_data_map",
            et.join("extension/named_data_map"),
            false,
        ),
        (
            "extension_threadpool",
            et.join("extension/threadpool"),
            false,
        ),
        ("executorch_core", et.clone(), false),
        ("portable_kernels", et.join("kernels/portable"), false),
        (
            "XNNPACK",
            et.join("backends/xnnpack/third-party/XNNPACK"),
            false,
        ),
        (
            "xnnpack-microkernels-prod",
            et.join("backends/xnnpack/third-party/XNNPACK"),
            false,
        ),
        (
            "pthreadpool",
            et.join("backends/xnnpack/third-party/pthreadpool"),
            false,
        ),
        (
            "cpuinfo",
            et.join("backends/xnnpack/third-party/cpuinfo"),
            false,
        ),
        ("flatccrt", et.join("third-party/flatcc_ep/lib"), false),
        ("swipe_decoder", build_dir, false),
    ];
    for (name, dir, whole) in libs {
        println!("cargo:rustc-link-search=native={}", dir.display());
        if *whole {
            println!("cargo:rustc-link-lib=static:+whole-archive={name}");
        } else {
            println!("cargo:rustc-link-lib=static={name}");
        }
    }

    // C++ runtime + system deps ExecuTorch/XNNPACK need (propagate too).
    println!("cargo:rustc-link-lib=dylib=stdc++");
    println!("cargo:rustc-link-lib=dylib=pthread");
    println!("cargo:rustc-link-lib=dylib=dl");
    println!("cargo:rustc-link-lib=dylib=m");

    println!("cargo:rustc-cfg=futo");
}
