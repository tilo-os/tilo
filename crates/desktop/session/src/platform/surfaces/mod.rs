//! The session's own surfaces on the Linux desktop: the widget trees, the shell and the shell's
//! parts it shows over and under the windows, named here so a role asks for them and never for the
//! crate that draws them.

pub use tilo_session_core::surface::Placement;

#[cfg(target_os = "linux")]
mod app_process;
#[cfg(target_os = "linux")]
mod desktop;
#[cfg(target_os = "linux")]
mod frames;
#[cfg(target_os = "linux")]
mod gestures;
#[cfg(target_os = "linux")]
mod raster;
#[cfg(target_os = "linux")]
mod well_policy;
#[cfg(target_os = "linux")]
mod widget;

/// What the session made `window`'s connection for, as the desktop's policy reads it: the app a
/// spare window process's connection was bound to names the window, and nothing else does.
#[cfg(target_os = "linux")]
pub(crate) fn made_for(
    window: &tilo_compositor::WindowInfo,
) -> Option<tilo_desktop_policy::MadeFor> {
    window.made_for.as_ref().map(|made| match made {
        tilo_compositor::MadeFor::Spare => tilo_desktop_policy::MadeFor::Spare,
        tilo_compositor::MadeFor::App { app, workspace } => tilo_desktop_policy::MadeFor::App {
            app: app.clone(),
            workspace: workspace.clone(),
        },
    })
}

/// The tilolang shell a desktop shows: the package it runs (the person's installed one or this
/// build's copy), and the identity a part of it is made with.
#[cfg(target_os = "linux")]
pub use crate::shell::part::{ShellIdentity, compile_shell, installed_shell};
/// The desktop's own surfaces, as a Linux session drives them: the toolkit's widget trees on their
/// layers ([`WidgetSurface`]), the desktop under the windows and its window policy
/// ([`ShellDesktop`], [`WellPolicy`]), a surface app on a layer of its own ([`AppProcess`]) and the
/// tilolang shell's parts and slot views.
#[cfg(target_os = "linux")]
pub use crate::shell::{
    part::{SessionLink, ShellPart, ShellWork},
    part_view,
};
#[cfg(target_os = "linux")]
pub use app_process::AppProcess;
#[cfg(target_os = "linux")]
pub use desktop::{ShellDesktop, desktop};
#[cfg(target_os = "linux")]
pub use gestures::ShellGesture;
#[cfg(target_os = "linux")]
pub use well_policy::{WellPolicy, WindowObserver};
#[cfg(target_os = "linux")]
pub use widget::WidgetSurface;
