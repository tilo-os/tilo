//! What a caller of the driver needs besides the app's own frames: History's held-back
//! controls, what a hosted component publishes, the pointers a host placed over the surface,
//! and the check that the layout it keeps is the layout a fresh frame builds.
//!
//! Every method here reads or drives the app's real state — its History host, its bound
//! publication, its presence — so a caller sees what a window would.

use super::Driver;
use crate::widget::Widget;

/// What the driver keeps of what is shown over its app: the pointers a host placed on the
/// surface, in the surface's logical pixels, and how often it checks its retained layout.
#[derive(Default)]
pub(super) struct Overlay {
    pointers: Vec<crate::presence::PeerCursor>,
    /// Check the retained layout against a fresh rebuild every this many frames that kept
    /// their tree; zero, the check is off.
    retained_geometry_check_every: u32,
    verifying_geometry: bool,
    retained_frames: u64,
}

// History's held-back controls and the actions hosted components publish: Tilo's model's.
#[cfg(feature = "model")]
impl Driver {
    // ---- History's held-back controls ----

    /// Mint a one-use token for the current presentation of one item History holds back.
    pub fn pending_history_control_token(
        &mut self,
        id: crate::history_mode::pending_controls::PendingControlId,
    ) -> Option<crate::history_mode::pending_controls::ContinuationToken> {
        self.app.history.as_mut()?.pending_control_token(id)
    }

    /// Continue one exact held-back control under the authority as it stands now. `None` while
    /// History holds the input, or when the item is no longer the presentation the token names.
    pub fn continue_pending_history_control(
        &mut self,
        token: &crate::history_mode::pending_controls::ContinuationToken,
    ) -> Option<crate::history_mode::pending_controls::PendingControlStatus> {
        if self.app.framework_input.get()
            || self
                .app
                .history
                .as_ref()
                .is_none_or(|history| history.captures_input())
        {
            return None;
        }
        let prepared = self.app.history.as_mut()?.prepare_pending_control(token)?;
        let settled = self.app.settle_history_control(prepared);
        self.app.needs_sync = true;
        self.app.layout.layout_dirty = true;
        self.run_frame();
        settled
    }

    /// Read History's review against `clock`, in Unix milliseconds, instead of the wall clock.
    #[cfg(test)]
    pub(crate) fn set_history_clock(&mut self, clock: fn() -> Option<u64>) {
        if let Some(history) = &mut self.app.history {
            history.set_clock(clock);
        }
    }

    /// Invoke only the app's own local History API (the window's History, its search, its edit
    /// and unsaved-changes hosts), never an app destination, then run the frame that shows it.
    ///
    /// # Errors
    /// When `address` names none of the app's local APIs, or the action fails.
    pub fn invoke_local_history_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Result<(), String> {
        let started = self
            .app
            .invoke_history_address_local(address, argument)
            .ok_or_else(|| format!("{address:?} is not one of the app's own local actions"))?;
        let _started = started.map_err(|error| error.to_string())?;
        self.idle();
        self.run_frame();
        Ok(())
    }

    // ---- what a hosted component publishes ----

    /// Invoke one explicit hosted component destination with the owner it currently has: the
    /// action `address` names at the instance `address` names, with `arguments`, then the frame
    /// that shows it. A view operation that reads the laid-out view is run after a frame lays
    /// it out.
    ///
    /// # Errors
    /// Refuses an address no mounted component publishes at that instance, a denied or
    /// unavailable action, and a failure the action reports.
    pub fn invoke_bound_action(
        &mut self,
        address: &crate::bound_dispatch::BoundActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let answered = crate::widget::resource_operation::laying_out_first(|| {
            self.app.invoke_bound_action_at(address, arguments)
        });
        let answered = match answered {
            Err(error) if crate::widget::resource_operation::asks_to_lay_out_first(&error) => {
                self.relayout();
                self.app.invoke_bound_action_at(address, arguments)
            }
            other => other,
        };
        self.run_frame();
        answered
    }

    /// The resources the app's hosted components publish to the workspace, as their permissions
    /// let them be invoked. With `instance`, only what that component instance publishes.
    ///
    /// # Errors
    /// Refuses a closed or unavailable component.
    pub fn published_bound_resources_of(
        &self,
        instance: Option<crate::application::InstanceId>,
    ) -> Result<Vec<tilo_actions::ResourceSpec>, tilo_actions::ApiError> {
        crate::bound_dispatch::discover_bound_resources(
            self.app.root_widget.as_ref(),
            &self.app.state.doc,
            None,
        )
        .map_err(|error| crate::bound_dispatch::BoundActionError::Dispatch(error).into_api())?
        .into_iter()
        .filter(|snapshot| instance.is_none_or(|id| snapshot.binding.instance_id == id))
        .flat_map(|snapshot| {
            let binding = snapshot.binding.clone();
            snapshot
                .into_resources()
                .into_iter()
                .filter_map(crate::bound_dispatch::published_here)
                .map(move |resource| {
                    self.app
                        .actions
                        .bound_publication
                        .project(&binding, resource)
                        .map_err(crate::bound_dispatch::BoundActionError::into_api)
                })
        })
        .collect()
    }

    /// Where the app shows `address` of the component `instance` of `app` over `record`: the one
    /// control's rect, as the frame clipped it, or none when nothing of the app shows it now.
    #[must_use]
    pub fn published_action_locus(
        &self,
        instance: crate::application::InstanceId,
        app: &str,
        record: &str,
        address: &tilo_actions::ActionAddress,
    ) -> Option<vello::kurbo::Rect> {
        if !self.action_geometry_visible() {
            return None;
        }
        let target = self.app.actions.bound_publication.target(address).ok()??;
        if target.binding.instance_id != instance
            || target.binding.app_id != app
            || target.binding.checkout.id.as_str() != record
        {
            return None;
        }
        let snapshots = crate::bound_dispatch::discover_bound_resources(
            self.app.root_widget.as_ref(),
            &self.app.state.doc,
            None,
        )
        .ok()?;
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.binding.same_component(&target.binding))?;
        let mut result = Vec::new();
        self.visit_publication_locus(
            self.app.root_widget.as_ref(),
            &target,
            snapshot.resources(),
            &mut result,
        );
        if result.len() == 1 {
            result.pop().flatten()
        } else {
            None
        }
    }

    /// Whether the app's action geometry shows: nothing stands over it that would hide it.
    fn action_geometry_visible(&self) -> bool {
        let app = &*self.app;
        !(app.close_admission.dialog.is_some()
            || app.palette_presented()
            || app
                .history
                .as_ref()
                .is_some_and(|history| !history.live_visible()))
    }

    fn visit_publication_locus(
        &self,
        widget: &dyn Widget,
        target: &crate::bound_dispatch::BoundActionTarget,
        resources: &[tilo_actions::ResourceSpec],
        result: &mut Vec<Option<vello::kurbo::Rect>>,
    ) {
        if crate::bound_dispatch::callback_binding(widget)
            .is_some_and(|binding| binding.same_component(&target.binding))
        {
            // Nested foreign owners cannot contribute a same-named control: their geometry is
            // observed through their own alias.
            fn foreign_owner(
                widget: &dyn Widget,
                owner: &crate::bound_dispatch::CallbackBinding,
            ) -> bool {
                if crate::bound_dispatch::callback_binding(widget)
                    .is_some_and(|binding| !binding.same_component(owner))
                {
                    return true;
                }
                let mut foreign = false;
                widget.for_each_child(&mut |child| {
                    foreign |= foreign_owner(child, owner);
                });
                foreign
            }
            if foreign_owner(widget, &target.binding) {
                result.push(None);
                return;
            }
            let mut tree = crate::a11y_tree::SemanticTree::new();
            tree.refresh_bound(
                widget,
                &self.app.layout.node_map,
                &self
                    .app
                    .bindings
                    .semantic_node_names(&self.app.layout.node_map),
                self.app.focused_node,
                resources,
            );
            let bindings = crate::widget::controls::control_action_bindings(widget);
            result.push(
                crate::action_locus::resource_bounds(&tree, &bindings, &target.address.address)
                    .and_then(|(bounds, node)| self.clipped_publication_bounds(bounds, node)),
            );
            return;
        }
        widget.for_each_child(&mut |child| {
            self.visit_publication_locus(child, target, resources, result);
        });
    }

    /// `bounds` of the widget `node`, clipped by every scroll region it stands in, when exactly
    /// one laid-out widget is that node.
    fn clipped_publication_bounds(
        &self,
        bounds: vello::kurbo::Rect,
        node: u64,
    ) -> Option<vello::kurbo::Rect> {
        let pointer = *self.app.layout.node_map.get(&taffy::NodeId::from(node))?;
        fn visit(
            widget: &dyn Widget,
            pointer: *const dyn Widget,
            bounds: vello::kurbo::Rect,
            matches: &mut Vec<vello::kurbo::Rect>,
        ) {
            if std::ptr::addr_eq(std::ptr::from_ref(widget), pointer) {
                matches.push(bounds);
                return;
            }
            let bounds = if let Some(clip) = widget.child_clip_rect() {
                if ![clip.x0, clip.y0, clip.x1, clip.y1]
                    .into_iter()
                    .all(f64::is_finite)
                    || clip.width() <= 0.0
                    || clip.height() <= 0.0
                {
                    return;
                }
                bounds.intersect(clip)
            } else {
                bounds
            };
            if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
                return;
            }
            widget.for_each_child(&mut |child| visit(child, pointer, bounds, matches));
        }
        let mut matches = Vec::new();
        visit(self.app.root_widget.as_ref(), pointer, bounds, &mut matches);
        (matches.len() == 1).then(|| matches[0])
    }
}

impl Driver {
    // ---- other people's pointers a host placed on the surface ----

    /// Place `pointers` over the app, in the surface's logical pixels: each is drawn with the
    /// arrow, name tag and colour the app's own presence gives that person. A pointer outside
    /// the surface is dropped. Whether anything changed.
    pub fn set_placed_pointers(&mut self, pointers: Vec<crate::presence::PeerCursor>) -> bool {
        let (width, height) = self.viewport();
        let (width, height) = (f64::from(width), f64::from(height));
        let pointers: Vec<_> = pointers
            .into_iter()
            .filter(|pointer| {
                (0.0..width).contains(&pointer.pos.0) && (0.0..height).contains(&pointer.pos.1)
            })
            .collect();
        if pointers.is_empty() && self.overlay.pointers.is_empty() {
            return false;
        }
        self.overlay.pointers = pointers;
        self.surface.redraw.set(true);
        true
    }

    /// The pointers [`Self::set_placed_pointers`] last kept.
    #[must_use]
    pub fn placed_pointers(&self) -> &[crate::presence::PeerCursor] {
        &self.overlay.pointers
    }

    /// The placed pointers as the last frame drew them: without the name of one whose tag would
    /// meet the caret tag of the same person.
    pub fn drawn_placed_pointers(&mut self) -> Vec<crate::presence::PeerCursor> {
        self.pointers_beside_caret_tags(vello::kurbo::Affine::scale(self.surface.scale.get()))
    }

    fn pointers_beside_caret_tags(
        &mut self,
        transform: vello::kurbo::Affine,
    ) -> Vec<crate::presence::PeerCursor> {
        let mut pointers = self.overlay.pointers.clone();
        if pointers.is_empty() {
            return pointers;
        }
        let to_surface = transform.inverse();
        let tags: Vec<_> = self
            .app
            .presence_ctx
            .caret_tags()
            .into_iter()
            .map(|tag| crate::presence::CaretTag {
                rect: to_surface.transform_rect_bbox(tag.rect),
                ..tag
            })
            .collect();
        let (width, height) = self.viewport();
        let viewport = vello::kurbo::Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
        crate::presence::hide_names_beside_caret_tags(
            &mut pointers,
            &tags,
            viewport,
            &mut self.app.text_renderer,
            &self.app.theme,
        );
        pointers
    }

    /// Draw the placed pointers over the frame just painted.
    pub(super) fn paint_placed_pointers(&mut self) {
        if self.overlay.pointers.is_empty() {
            return;
        }
        let transform = vello::kurbo::Affine::scale(self.surface.scale.get());
        let pointers = self.pointers_beside_caret_tags(transform);
        let (width, height) = self.viewport();
        crate::presence::draw_peer_cursors(
            &mut self.scene,
            transform,
            vello::kurbo::Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            &mut self.app.text_renderer,
            &self.app.theme,
            &pointers,
        );
    }

    // ---- the retained layout ----

    /// Check the retained layout against a fresh rebuild every `every` frames that kept their
    /// tree, and report a divergence where it is found. Off in a build without debug assertions.
    pub fn set_retained_geometry_check(&mut self, every: u32) {
        self.overlay.retained_geometry_check_every = if cfg!(debug_assertions) { every } else { 0 };
    }

    /// Count a frame that kept its layout tree, as the retained-geometry check's clock.
    pub(super) fn note_retained_frame(&mut self) {
        if self.overlay.verifying_geometry || self.app.layout.last_pass.rebuilt {
            return;
        }
        self.overlay.retained_frames = self.overlay.retained_frames.wrapping_add(1);
    }

    /// Compare the retained layout with a fresh rebuild when the check
    /// ([`Self::set_retained_geometry_check`]) is due, and report a divergence.
    pub(super) fn check_retained_geometry_if_due(&mut self) {
        let every = self.overlay.retained_geometry_check_every;
        if every == 0
            || self.overlay.verifying_geometry
            || self.overlay.retained_frames == 0
            || !self
                .overlay
                .retained_frames
                .is_multiple_of(u64::from(every))
        {
            return;
        }
        self.overlay.verifying_geometry = true;
        if let Err(mismatch) = self.verify_retained_geometry() {
            tracing::error!(%mismatch, "display-less driver: retained layout diverged from a fresh rebuild");
            debug_assert!(
                false,
                "retained layout diverged from a fresh rebuild: {mismatch}"
            );
        }
        self.overlay.verifying_geometry = false;
    }

    // ---- profiling ----

    /// Stop timing the app's frames.
    pub fn end_profiling(&mut self) {
        self.app.profiler = None;
    }
}
