//! Effect handlers that answer on the window a view laid out.

use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_resource::{Effect, Resource};
use tilo_ui_base::host_operation::{ResourceOperation, ResourceOperationStart};
use tilo_ui_base::widget::Widget;

use std::rc::Weak;

use super::{
    EffectAnswer, EffectCx, EffectRunner, Effects, Jobs, OwnedJobs, Registered, ViewHandler,
};
use crate::framework::surface::SurfaceScope;

/// The lifetime of a window's work, as a part of the view that laid the
/// window out: what it started is stopped once the part is gone and its
/// registration let go.
pub(super) struct WindowScope(pub(super) std::rc::Rc<SurfaceScope>);

impl WindowScope {
    pub(super) fn new(alive: &std::rc::Weak<()>) -> Self {
        Self(SurfaceScope::new(alive.clone()))
    }
}

impl Drop for WindowScope {
    fn drop(&mut self) {
        self.0.retire();
    }
}

type WindowHandle<R, E, W, H> = Box<dyn FnMut(&mut W, E, &mut EffectCx<R, H>) -> EffectAnswer>;

/// A view's effect handler that answers on the window a view laid out
/// ([`ViewCx::window_effects`](crate::framework::ViewCx::window_effects)): it finds
/// the window `W` in the view's tree as the effect starts and lends it to
/// the handler for that call. What finishes later (a job's completion)
/// reaches state the window shares, as for any view handler.
pub(super) struct WindowHandler<R: Resource, E, W> {
    pub(super) runner: EffectRunner<R, (), E>,
    pub(super) handle: WindowHandle<R, E, W, ()>,
}

impl<R: Resource, E: Effect, W: Widget + 'static> ViewHandler for WindowHandler<R, E, W> {
    fn start(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &tilo_resource::Doc,
        view: &mut dyn Widget,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if op.request().type_id != E::OPERATION {
            return Ok(None);
        }
        let window = tilo_ui_base::widget::find_mut::<W>(view).ok_or("the window is not open")?;
        let handle = &mut self.handle;
        self.runner.start(op, &mut (), Some(doc), |(), effect, cx| {
            handle(window, effect, cx)
        })
    }

    fn poll(
        &mut self,
        invocation: InvocationId,
        cancel: bool,
        _view: &mut dyn Widget,
    ) -> Option<InvocationUpdate> {
        self.runner.poll(invocation, cancel, &mut ())
    }
}

/// A view's effect handler whose window hosts the work
/// ([`ViewCx::window_host_effects`](crate::framework::ViewCx::window_host_effects)):
/// the window `W` is lent to the handler as it starts an effect, and again
/// to the completions of the jobs it started, as a hosted surface's
/// completions get the surface.
pub(super) struct HostedWindowHandler<R: Resource, E, W> {
    pub(super) runner: EffectRunner<R, W, E>,
    pub(super) handle: WindowHandle<R, E, W, W>,
    pub(super) scope: WindowScope,
}

impl<R: Resource, E: Effect, W: Widget + 'static> ViewHandler for HostedWindowHandler<R, E, W> {
    fn start(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &tilo_resource::Doc,
        view: &mut dyn Widget,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if op.request().type_id != E::OPERATION {
            return Ok(None);
        }
        let window = tilo_ui_base::widget::find_mut::<W>(view).ok_or("the window is not open")?;
        self.runner.start(op, window, Some(doc), &mut self.handle)
    }

    /// While the window is not in the view (a part switched away), its
    /// work waits for it: the completion needs the window.
    fn poll(
        &mut self,
        invocation: InvocationId,
        cancel: bool,
        view: &mut dyn Widget,
    ) -> Option<InvocationUpdate> {
        if !self.scope.0.active() {
            return None;
        }
        let window = tilo_ui_base::widget::find_mut::<W>(view)?;
        self.runner.poll(invocation, cancel, window)
    }
}

/// The jobs a window starts on its own
/// ([`ViewCx::window_jobs`](crate::framework::ViewCx::window_jobs)), whose
/// completions get the window `W`.
pub(super) struct WindowJobs<R: Resource, W> {
    pub(super) jobs: OwnedJobs<R, W>,
    pub(super) scope: WindowScope,
}

impl<R: Resource, W: Widget + 'static> ViewHandler for WindowJobs<R, W> {
    fn start(
        &mut self,
        _op: &ResourceOperation<'_>,
        _doc: &tilo_resource::Doc,
        _view: &mut dyn Widget,
    ) -> Result<Option<ResourceOperationStart>, String> {
        Ok(None)
    }

    fn poll(
        &mut self,
        _invocation: InvocationId,
        _cancel: bool,
        _view: &mut dyn Widget,
    ) -> Option<InvocationUpdate> {
        None
    }

    /// While the window is not in the view (a part switched away), what
    /// its jobs finished waits for it: the completion needs the window.
    fn poll_jobs(&mut self, view: &mut dyn Widget) {
        if self.scope.0.active()
            && let Some(window) = tilo_ui_base::widget::find_mut::<W>(view)
        {
            self.jobs.poll(window);
        }
    }
}

/// Registering the handlers of a view's windows.
impl<R: Resource> Effects<R> {
    pub(crate) fn handle_in_window<E: Effect, W: Widget + 'static>(
        &self,
        alive: Weak<()>,
        handle: impl FnMut(&mut W, E, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) {
        let Some(env) = self.env.clone() else {
            return;
        };
        self.inner.borrow_mut().handlers.push(Registered {
            alive,
            operation: E::OPERATION,
            handler: Box::new(WindowHandler {
                runner: EffectRunner::<R, (), E>::new(env),
                handle: Box::new(handle),
            }),
        });
    }

    pub(crate) fn handle_hosted_in_window<E: Effect, W: Widget + 'static>(
        &self,
        alive: Weak<()>,
        handle: impl FnMut(&mut W, E, &mut EffectCx<R, W>) -> EffectAnswer + 'static,
    ) {
        let Some(env) = self.env.clone() else {
            return;
        };
        let scope = WindowScope::new(&alive);
        self.inner.borrow_mut().handlers.push(Registered {
            alive,
            operation: E::OPERATION,
            handler: Box::new(HostedWindowHandler {
                runner: EffectRunner::<R, W, E>::new(env.scoped(&scope.0)),
                handle: Box::new(handle),
                scope,
            }),
        });
    }

    /// Jobs for the window `W` while `alive` lives: polled with the window
    /// before the view syncs, stopped once `alive` is gone.
    pub(crate) fn window_jobs<W: Widget + 'static>(&self, alive: Weak<()>) -> Jobs<R> {
        let scope = WindowScope::new(&alive);
        let jobs = self.scoped_surface_jobs(&scope.0);
        if self.env.is_some() {
            self.inner.borrow_mut().handlers.push(Registered {
                alive,
                // Answers no effect: the window's jobs are nobody's invocation.
                operation: "",
                handler: Box::new(WindowJobs {
                    jobs: OwnedJobs::<R, W>::new(jobs.clone()),
                    scope,
                }),
            });
        }
        jobs
    }
}
