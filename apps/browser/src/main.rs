//! Thin binary entry point: the [`browser::BrowserShell`] framework app.
//! Every launch mode — a window showing one page, a private window
//! (`--private`), a page named on the command line, `--shots` scenarios and
//! the live-engine captures (`--shots-live`) — is the framework's.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    // The SHM engine path reads WebKit's frames back on the CPU, yet with a
    // real display WebKit still tries hardware GL for its compositor. On a
    // machine whose GPU EGL is unreliable (e.g. a nested NVIDIA node
    // reporting `driver (null)`) that context can wedge mid-render under
    // load — the page freezes and never recovers while the chrome stays
    // live. Pin WebKit's GL to software so the SHM path is reliable; the
    // zero-copy dmabuf path (opt-in) keeps hardware. Overridable by setting
    // the var yourself. wgpu (the chrome) uses Vulkan, so it's unaffected.
    #[cfg(feature = "wpe")]
    if std::env::var_os("TILO_WEBVIEW_DMABUF").is_none()
        && std::env::var_os("LIBGL_ALWAYS_SOFTWARE").is_none()
    {
        // Safe: called at the very top of `main`, before any thread (or the
        // WebKit WebProcess) that could read the environment concurrently.
        unsafe { std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1") };
    }
    ui::framework::run::<browser::BrowserShell>();
    // Every Browser tab of this profile browses through one Chromium host.
    // When it is this process's, the window closing must not take the
    // other tabs' pages down with it.
    ui::webview::chromium::serve_shared_until_idle();
}
