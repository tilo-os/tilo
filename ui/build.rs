fn main() {
    // The WPE WebKit backend (feature `webview-wpe`) links the WPE
    // stack. Fedora ships no WPE packages — build them with
    // os/dev/build-wpe.sh and point PKG_CONFIG_PATH at the prefix.
    // Without the feature this script is a no-op, so the workspace
    // builds on machines without the libraries.
    if std::env::var_os("CARGO_FEATURE_WEBVIEW_WPE").is_none() {
        return;
    }
    for lib in [
        "wpe-webkit-2.0",
        "wpebackend-fdo-1.0",
        "wpe-1.0",
        "wayland-server",
        "glib-2.0",
        "gobject-2.0",
        // DMA-BUF zero-copy path (QOS_WEBVIEW_DMABUF=1): EGL to receive
        // WebKit's exported EGLImage + gbm to open the render node.
        "egl",
        "gbm",
    ] {
        if let Err(err) = pkg_config::probe_library(lib) {
            // Cargo's own channel, not a panic. `cargo::error=` fails the
            // build with this one line; panicking buries it under a
            // build-script backtrace, which is how a missing library reads as
            // a compiler crash rather than as the one-line instruction it is.
            println!(
                "cargo::error=feature `webview-wpe` needs {lib} (build the stack with \
                 os/dev/build-wpe.sh and set PKG_CONFIG_PATH to the prefix): {err}"
            );
        }
    }
}
