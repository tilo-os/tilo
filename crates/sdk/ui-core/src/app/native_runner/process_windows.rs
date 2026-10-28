//! Every window of the process on one loop.
//!
//! A process shows the window it was launched with and every window the
//! shell opens in it later ([`crate::window_door`]). Each is an
//! [`AppHandler`] of its own, with its own app, window, renderer and
//! connections; this hands each the events of its windows, and the events of
//! no window to all of them or to the one in front. A window that is done
//! closes alone; the last one ends the loop, as a process's only window
//! always has.

use super::{App, AppHandler, UserEvent};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::WindowId;

/// Builds the window the shell asked for, or says why it cannot.
pub(crate) type OpenWindow<'a> = dyn FnMut(crate::window_door::PendingWindow) -> Option<App> + 'a;

/// Read the window door the launcher handed this process, if it was handed
/// one, waking the loop behind `proxy` with each window the shell asks for.
pub(super) fn serve_door(proxy: &EventLoopProxy<UserEvent>) -> Option<()> {
    let door = crate::window_door::take_door()?;
    let proxy = proxy.clone();
    crate::window_door::serve(door, move |pending| {
        proxy.send_event(UserEvent::OpenWindow(pending)).is_ok()
    })
    .map_err(|error| tracing::warn!(%error, "the shell cannot open more windows here"))
    .ok()
}

pub(super) struct ProcessWindows<'a, 'b> {
    /// The launch's window first, while it is open.
    windows: Vec<AppHandler>,
    proxy: EventLoopProxy<UserEvent>,
    open: Option<&'a mut OpenWindow<'b>>,
    /// The window that last had the keyboard: input of no window goes to it.
    focused: Option<WindowId>,
}

impl<'a, 'b> ProcessWindows<'a, 'b> {
    pub(super) fn new(
        first: AppHandler,
        proxy: EventLoopProxy<UserEvent>,
        open: Option<&'a mut OpenWindow<'b>>,
    ) -> Self {
        Self {
            windows: vec![first],
            proxy,
            open,
            focused: None,
        }
    }

    /// The window left when the loop ended: the launch's, unless it closed
    /// before others did.
    pub(super) fn into_first(self) -> Option<AppHandler> {
        self.windows.into_iter().next()
    }

    fn owner(&self, window_id: WindowId) -> Option<usize> {
        self.windows
            .iter()
            .position(|handler| handler.shows(window_id))
    }

    /// The window input of no window goes to: the one in front, else the
    /// launch's.
    fn front(&self) -> usize {
        self.focused
            .and_then(|focused| self.owner(focused))
            .unwrap_or(0)
    }

    /// Close every window that is done. The last one ends the loop instead,
    /// and is kept: the loop's end saves it, as it always has.
    fn close_finished(&mut self, event_loop: &ActiveEventLoop) {
        while let Some(index) = self
            .windows
            .iter()
            .position(|handler| handler.closing.get())
        {
            if self.windows.len() == 1 {
                event_loop.exit();
                return;
            }
            let mut closed = self.windows.remove(index);
            closed.exiting(event_loop);
            drop(closed);
        }
    }

    fn open_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        pending: crate::window_door::PendingWindow,
    ) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let Some(mut app) = open(pending) else {
            return;
        };
        app.prepare_native_presentation(crate::host_chrome::HostChrome::current(
            app.input.platform,
        ));
        let mut handler = app.into_handler(&self.proxy, None, |_| {});
        handler.resumed(event_loop);
        self.windows.push(handler);
    }
}

impl ApplicationHandler<UserEvent> for ProcessWindows<'_, '_> {
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::OpenWindow(pending) => self.open_window(event_loop, pending),
            // Each app takes the wakes of its own scope and ignores the rest.
            event @ (UserEvent::Wake(_)
            | UserEvent::DevSessionStop
            | UserEvent::DevSessionReload) => {
                for handler in &mut self.windows {
                    handler.user_event(event_loop, event.clone());
                }
            }
            UserEvent::RenderReady(window_id) => {
                let index = self.owner(window_id).unwrap_or(0);
                if let Some(handler) = self.windows.get_mut(index) {
                    handler.user_event(event_loop, UserEvent::RenderReady(window_id));
                }
            }
            #[cfg(feature = "native")]
            event @ UserEvent::AccessKit { window_id, .. } => {
                let index = self.owner(window_id).unwrap_or(0);
                if let Some(handler) = self.windows.get_mut(index) {
                    handler.user_event(event_loop, event);
                }
            }
            event => {
                let index = self.front();
                if let Some(handler) = self.windows.get_mut(index) {
                    handler.user_event(event_loop, event);
                }
            }
        }
        self.close_finished(event_loop);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        for handler in &mut self.windows {
            handler.resumed(event_loop);
        }
        self.close_finished(event_loop);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        for handler in &mut self.windows {
            handler.suspended(event_loop);
        }
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        for handler in &mut self.windows {
            handler.exiting(event_loop);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if matches!(event, WindowEvent::Focused(true)) {
            self.focused = Some(window_id);
        }
        // A window no app shows here (one being torn down) goes to the
        // launch's, as every window did when it was the only one.
        let index = self.owner(window_id).unwrap_or(0);
        if let Some(handler) = self.windows.get_mut(index) {
            handler.window_event(event_loop, window_id, event);
        }
        self.close_finished(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        for handler in &mut self.windows {
            handler.about_to_wait(event_loop);
        }
        self.close_finished(event_loop);
    }
}

impl AppHandler {
    /// Whether `window_id` is one of this app's windows: its own, a surface
    /// it floats out of it, or its audience view.
    fn shows(&self, window_id: WindowId) -> bool {
        self.window
            .as_ref()
            .is_some_and(|window| window.id() == window_id)
            || self
                .floating_windows
                .iter()
                .any(|surface| surface.window.id() == window_id)
            || self
                .audience_presentation
                .as_ref()
                .is_some_and(|audience| audience.window.id() == window_id)
    }
}
