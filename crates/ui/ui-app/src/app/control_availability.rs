// Pushing what each control may run into the retained tree: the walks a
// frame takes only when what they read moved.

impl App {
    /// Push what each control in the tree may run now into it. The walks
    /// of the tree here read only the tree, the document and permissions it
    /// was built from, what the window's own History API serves and
    /// whether input goes to History: a frame that moved none of them (one
    /// only an animation asked for) walks nothing. New verdicts from the
    /// device move the tree generation.
    fn refresh_control_action_availability(&mut self) {
        #[cfg(feature = "model")]
        let (history_captures, history_api) = (
            self.history.as_ref().is_some_and(|h| h.captures_input()),
            self.refresh_history_controls(),
        );
        #[cfg(not(feature = "model"))]
        let (history_captures, history_api) = (false, std::collections::HashSet::new());
        tilo_startup_metrics::startup_mark("actions-history-done");
        let inputs = runtime::AvailabilityInputs {
            tree: self.tree_generation(),
            framework_input: self.framework_input.get(),
            history_captures,
            history_api,
        };
        let unchanged = self.layout.walks.availability.as_ref().filter(|walk| {
            walk.inputs == inputs && walk.controls == self.actions.last_control_availability
        });
        if let Some(walk) = unchanged {
            if walk.unread {
                let history_api = inputs.history_api.clone();
                let _ = self.refresh_bound_control_availability(&history_api);
            }
            return;
        }
        #[cfg(test)]
        let _walk = crate::frame_probe::span("walk.availability");
        let history_api = inputs.history_api.clone();
        let bound = self.refresh_bound_control_availability(&history_api);
        self.refresh_emitter_availability(history_api);
        // What each control may run is what assistive technology is offered:
        // a walk for verdicts the device changed changes it with nothing in
        // the tree marking it.
        self.mark_semantic_tree_stale();
        self.layout.walks.availability = Some(runtime::AvailabilityWalk {
            inputs,
            controls: self.actions.last_control_availability.clone(),
            unread: bound.unread,
        });
    }

    /// Whether what the last walk pushed into the controls is out of date
    /// with nothing in the tree changed: the device sent new verdicts. A
    /// frame walks the tree again.
    #[cfg(all(feature = "native", feature = "model"))]
    pub(crate) fn permissions_walk_due(&self) -> bool {
        self.layout.walks.availability.as_ref().is_some_and(|walk| {
            walk.inputs.tree.shared_permissions != shared_permissions_revision()
        })
    }

    /// Push what the bound components' controls may run into them. Only Tilo's model hosts a
    /// component whose controls follow permissions of its own.
    fn refresh_bound_control_availability(
        &mut self,
        history_api: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) -> BoundAvailability {
        #[cfg(feature = "model")]
        let bound = self.project_bound_control_availability(history_api);
        #[cfg(not(feature = "model"))]
        let bound = {
            let _ = history_api;
            BoundAvailability { unread: false }
        };
        bound
    }

    #[cfg(feature = "model")]
    fn project_bound_control_availability(
        &mut self,
        history_api: &std::collections::HashSet<tilo_actions::ActionAddress>,
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
            history_api,
        );
        trace.finish(CacheResult::NotApplicable);
        tilo_startup_metrics::startup_mark("actions-bound-projection-done");
        BoundAvailability { unread }
    }

    /// Push the local action surface into the tree's action emitters.
    fn refresh_emitter_availability(
        &mut self,
        history_api: std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        // Only ActionEmitter controls consume this address projection. Semantic,
        // document and legacy controls still use their normal invocation APIs.
        // Recheck presence: widget-local changes can add/remove emitters.
        if !tilo_ui_base::widget::controls::has_action_control_bindings(self.root_widget.as_ref()) {
            // A replacement emitter must not inherit a prior refresh stamp.
            self.actions.last_control_availability = None;
            return;
        }
        // Without Tilo's model a window publishes no actions: a control bound to one runs
        // nothing, and shows it can't.
        #[cfg(not(feature = "model"))]
        {
            let _ = history_api;
            tilo_ui_base::widget::controls::refresh_control_action_availability(
                self.root_widget.as_mut(),
                &std::collections::HashSet::new(),
            );
        }
        #[cfg(feature = "model")]
        self.refresh_published_emitter_availability(history_api);
    }

    #[cfg(feature = "model")]
    fn refresh_published_emitter_availability(
        &mut self,
        history_api: std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        use crate::profiling::resource_trace::{CacheResult, Stage};
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
        allowed.insert(crate::widget::search_request::search_address());
        // So is the window's History API, wherever History serves it.
        allowed.extend(history_api);
        tilo_ui_base::widget::controls::refresh_control_action_availability(
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
}
