//! A headless compositor composites on OpenGL ES whichever client API its thread bound last. EGL
//! keeps the current API per thread: a session makes its EGL display on a thread beside its build,
//! and wgpu's GL backend binds desktop OpenGL on the thread that makes its adapter. A context made
//! for desktop OpenGL lacks the GLES extensions the renderer needs, and the session had no
//! compositor at all.

#![cfg(target_os = "linux")]

use tilo_compositor::StackPolicy;
use tilo_compositor::backend::headless::warm_display;

use crate::harness::headless;

type Egl = khronos_egl::DynamicInstance<khronos_egl::EGL1_4>;

#[test]
fn a_compositor_started_where_desktop_gl_was_bound_draws_on_gles() {
    // The display is made on another thread, as a session's start makes it.
    std::thread::spawn(warm_display)
        .join()
        .expect("the display thread ran");
    // SAFETY: loads the system's libEGL to set this thread's client API, as wgpu's GL backend does.
    let lib = unsafe { Egl::load_required() }.expect("libEGL");
    lib.bind_api(khronos_egl::OPENGL_API)
        .expect("desktop OpenGL is bindable");

    let mut compositor = headless(Box::new(StackPolicy));
    compositor.render().expect("a frame");
}
