// Pushing what each control may run into the retained tree: the walks a
// frame takes only when what they read moved.

impl App {
    /// Push what each control in the tree may run now into it. The walks
    /// of the tree here read only the tree, the document and permissions it
    /// was built from, what the window's own History door serves and
    /// whether input goes to History: a frame that moved none of them (one
    /// only an animation asked for) walks nothing, unless a bound
    /// component's permissions changed with time since
    /// ([`runtime::AvailabilityWalk::changes_at`]). An outside authority
    /// they follow announces its changes, which move the tree generation.
    fn refresh_control_action_availability(&mut self) {
        let history_captures = self.history.as_ref().is_some_and(|h| h.captures_input());
        let history_door = self.refresh_history_controls();
        tilo_startup_metrics::startup_mark("actions-history-done");
        let inputs = runtime::AvailabilityInputs {
            tree: self.tree_generation(),
            framework_input: self.framework_input.get(),
            history_captures,
            history_door,
        };
        let now = crate::time::now();
        let unchanged = self.layout.walks.availability.as_ref().filter(|walk| {
            walk.inputs == inputs
                && walk.controls == self.actions.last_control_availability
                && walk.changes_at.is_none_or(|at| now < at)
        });
        if let Some(walk) = unchanged {
            if walk.unread {
                let history_door = inputs.history_door.clone();
                let _ = self.refresh_bound_control_availability(&history_door);
            }
            return;
        }
        #[cfg(test)]
        let _walk = crate::profiling::frame_probe::span("walk.availability");
        let history_door = inputs.history_door.clone();
        let bound = self.refresh_bound_control_availability(&history_door);
        self.refresh_emitter_availability(history_door);
        // What each control may run is what assistive technology is offered:
        // a walk for permissions that changed (with time, or announced from
        // outside) changes it with nothing in the tree marking it.
        self.mark_semantic_tree_stale();
        self.layout.walks.availability = Some(runtime::AvailabilityWalk {
            inputs,
            controls: self.actions.last_control_availability.clone(),
            unread: bound.unread,
            changes_at: bound.changes_at,
        });
    }

    /// When a bound component's permissions next change with time alone,
    /// as the last walk of the tree found: a frame then walks it again.
    #[cfg(any(feature = "native", test))]
    pub(crate) fn permissions_change_at(&self) -> Option<crate::time::Instant> {
        self.layout
            .walks
            .availability
            .as_ref()
            .and_then(|walk| walk.changes_at)
    }

    /// Whether what the last walk pushed into the controls is out of date
    /// by `now` with nothing in the tree changed: a bound component's
    /// permissions changed with time ([`Self::permissions_change_at`]), or
    /// an outside authority they follow announced a change. A frame walks
    /// the tree again.
    #[cfg(feature = "native")]
    pub(crate) fn permissions_walk_due(&self, now: crate::time::Instant) -> bool {
        self.layout.walks.availability.as_ref().is_some_and(|walk| {
            walk.changes_at.is_some_and(|at| at <= now)
                || walk.inputs.tree.shared_permissions != crate::permissions::shared_revision()
        })
    }

    /// Push what the bound components' controls may run into them.
    fn refresh_bound_control_availability(
        &mut self,
        history_door: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) -> BoundAvailability {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        let trace = self.trace_resource_stage(Stage::BoundDiscovery);
        let discovered = crate::bound_dispatch::discover_cached_bound_resources(
            self.root_widget.as_ref(),
            &self.state.doc,
            &self.actions.bound_resource_cache,
            self.actions.resource_generation.get(),
        );
        let unread = discovered.is_err();
        let snapshots = discovered.unwrap_or_default();
        trace.finish(CacheResult::NotObserved);
        tilo_startup_metrics::startup_mark("actions-bound-discovery-done");
        let trace = self.trace_resource_stage(Stage::BoundControlProjection);
        let _ = crate::bound_dispatch::refresh_bound_controls(
            self.root_widget.as_mut(),
            &snapshots,
            history_door,
        );
        trace.finish(CacheResult::NotApplicable);
        tilo_startup_metrics::startup_mark("actions-bound-projection-done");
        let now = crate::time::now();
        let changes_at = snapshots
            .iter()
            .filter_map(|snapshot| {
                let change = snapshot.binding.permissions.borrow().next_change_in()?;
                Some(now + change)
            })
            .min();
        BoundAvailability { unread, changes_at }
    }

    /// Push the local action surface into the tree's action emitters.
    fn refresh_emitter_availability(
        &mut self,
        history_door: std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        use crate::profiling::resource_trace::{CacheResult, Stage};
        // Only ActionEmitter controls consume this address projection. Semantic,
        // document and legacy controls still use their normal invocation doors.
        // Recheck presence: widget-local changes can add/remove emitters.
        if !crate::widget::controls::has_action_control_bindings(self.root_widget.as_ref()) {
            // A replacement emitter must not inherit a prior refresh stamp.
            self.actions.last_control_availability = None;
            return;
        }
        let cache_key = self.ensure_local_control_cache();
        tilo_startup_metrics::startup_mark("actions-local-cache-done");
        let trace = self.trace_resource_stage(Stage::ControlCache);
        if self.actions.last_control_availability.as_ref() == Some(&cache_key) {
            trace.finish(CacheResult::Hit);
            return;
        }
        trace.finish(CacheResult::Miss);
        let trace = self.trace_resource_stage(Stage::ControlRefresh);
        // Clone only the compact address set so the cache borrow does not
        // span a mutable call into the retained widget tree.
        let mut allowed = self
            .actions
            .local_resource_cache
            .borrow()
            .as_ref()
            .map_or_else(std::collections::HashSet::new, |cache| {
                cache.allowed.clone()
            });
        // Opening search is the host's, and this host always answers it.
        allowed.insert(crate::search::search_address());
        // So is the window's History door, wherever History serves it.
        allowed.extend(history_door);
        crate::widget::controls::refresh_control_action_availability(
            self.root_widget.as_mut(),
            &allowed,
        );
        self.actions.last_control_availability = Some(cache_key);
        trace.finish(CacheResult::NotApplicable);
    }
}

/// What pushing availability into the bound components found.
struct BoundAvailability {
    /// Their resources could not be read: the next frame tries again.
    unread: bool,
    /// When their permissions next change with time alone.
    changes_at: Option<crate::time::Instant>,
}
