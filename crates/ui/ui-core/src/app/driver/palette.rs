//! The window's search, presence and navigation a driven app shows, and the shell it runs.
//!
//! Everything here reads or drives the app's own state: its palette, the presence its peers sent,
//! its semantic tree and the root it routes input to. The driver keeps none of it.

use vello::kurbo::Rect;

use super::Driver;
use crate::permissions::MemberId;
use crate::widget::Widget;

#[cfg(feature = "model")]
/// Where a view that follows someone has gone for their newest action: each action is gone to
/// once, so the follower can look around until the next one. Held by whoever follows, and handed
/// to [`Driver::follow_view`] each frame.
#[derive(Clone, Debug, Default)]
pub struct FollowedView {
    pub(crate) invocation: Option<tilo_actions::InvocationId>,
    pub(crate) shown: bool,
    pub(crate) opened: bool,
}

// The window's search: the action palette, which only Tilo's model has.
#[cfg(feature = "model")]
impl Driver {
    /// Whether the window's search shows, as the person sees it.
    #[must_use]
    pub fn palette_shown(&self) -> bool {
        self.app
            .action_palette()
            .is_some_and(crate::action_palette::ActionPalette::is_presented)
    }

    /// The rows the window's search shows, as a person reads them: each row's label and the line
    /// under it, in display order. Empty while it is closed.
    #[must_use]
    pub fn palette_rows(&self) -> Vec<(String, Option<String>)> {
        self.app
            .action_palette()
            .filter(|palette| palette.is_presented())
            .map(crate::action_palette::ActionPalette::result_rows)
            .unwrap_or_default()
    }

    /// The results the window's search shows, whole, in display order: each row's title, the
    /// picture it paints and what it runs (a command's chord among it). Empty while it is closed.
    #[must_use]
    pub fn palette_results(&self) -> Vec<crate::search::SearchResult> {
        self.app
            .action_palette()
            .filter(|palette| palette.is_presented())
            .map(crate::action_palette::ActionPalette::result_hits)
            .unwrap_or_default()
    }
}

// Presence: where the person is, and where others act.
impl Driver {
    #[cfg(feature = "model")]
    /// What the focused widget publishes as presence (its caret or selection): what the app tells
    /// its peers of where the person is.
    #[must_use]
    pub fn focus_presence(
        &self,
    ) -> Option<(
        crate::presence::PresenceKey,
        crate::collaboration::PresencePayload,
    )> {
        self.app.focus_presence()
    }

    #[cfg(feature = "model")]
    /// Where the app shows `interaction` now: the words an agent's caret or selection is on in
    /// text, else the one control its action is bound to. `None` when no single place shows it, or
    /// when something stands over the app (its search, a History review, the close dialog).
    #[must_use]
    pub fn semantic_locus(&self, interaction: &tilo_actions::SemanticInteraction) -> Option<Rect> {
        if !interaction.actor.is_person()
            && let Some(member) = interaction.actor.by.member_id()
            && let Some((_, rect)) = self.text_locus(member)
        {
            return Some(rect);
        }
        let bindings = self.action_bindings()?;
        crate::action_locus::interaction_bounds(self.app.semantic_tree(), &bindings, interaction)
    }

    #[cfg(feature = "model")]
    /// Bring the view to where `actor` acts, while it follows them: the text their caret or
    /// selection is in scrolls into view; else the control their newest action is bound to; where
    /// no view here shows it (another sheet, tab or page), the nearest view its resource publishes
    /// is opened through its own action. Each action is gone to once ([`FollowedView`]). Whether
    /// the view went there; a frame that finds nothing new changes nothing.
    pub fn follow_view(&mut self, actor: &MemberId, followed: &mut FollowedView) -> bool {
        let Some(interaction) = self
            .app
            .presence_ctx
            .agent_marker_interactions(crate::time::now())
            .into_iter()
            .find(|interaction| interaction.actor.by.member_id() == Some(actor))
        else {
            return false;
        };
        if followed.invocation != Some(interaction.invocation) {
            *followed = FollowedView {
                invocation: Some(interaction.invocation),
                ..FollowedView::default()
            };
        }
        if followed.shown {
            return false;
        }
        if let Some((widget, rect)) = self.text_locus(actor) {
            followed.shown = true;
            return self.reveal(widget, rect);
        }
        let locus = self.action_bindings().and_then(|bindings| {
            let node = crate::action_locus::interaction_node(
                self.app.semantic_tree(),
                &bindings,
                &interaction,
            )?;
            Some((node.widget_taffy_id?, node.bounds?))
        });
        if let Some((owner, bounds)) = locus {
            followed.shown = true;
            let Some(&widget) = self.app.layout.node_map.get(&taffy::NodeId::from(owner)) else {
                return false;
            };
            return self.reveal(widget.cast_const(), bounds);
        }
        if followed.opened {
            return false;
        }
        followed.opened = true;
        self.open_view_of(interaction.resource.id.as_str())
    }

    #[cfg(feature = "model")]
    /// Where the app's frame shows `actor`'s caret or selection in text: the widget that draws it
    /// and its box.
    fn text_locus(&self, actor: &MemberId) -> Option<(*const dyn Widget, Rect)> {
        let (key, range) =
            crate::presence::capability::actor_text_range(&self.app.presence_ctx, actor)?;
        crate::presence::capability::range_locus(
            self.app.root_widget.as_ref(),
            &self.app.theme,
            key,
            range,
        )
    }

    #[cfg(feature = "model")]
    /// The action bindings of the app's controls as they are laid out now; `None` while something
    /// stands over the app, whose controls then show nothing.
    fn action_bindings(&self) -> Option<Vec<crate::widget::ControlBindingMetadata>> {
        let app = &*self.app;
        let covered = app.palette_presented()
            || app.close_admission.dialog.is_some()
            || app
                .history
                .as_ref()
                .is_some_and(|history| !history.live_visible());
        if covered {
            return None;
        }
        app.with_native_presentation(|root, _, _, _, _| {
            crate::widget::controls::control_action_bindings(root)
        })
        .ok()
    }

    #[cfg(feature = "model")]
    /// Scroll `rect` of `target` into every viewport that shows it, and run the frame that shows
    /// it there when something moved.
    fn reveal(&mut self, target: *const dyn Widget, rect: Rect) -> bool {
        let moved = self.app.root_widget.reveal_descendant(target, rect);
        if moved {
            self.app.layout.layout_dirty = true;
            self.app.needs_sync = true;
            self.app.mark_semantic_tree_stale();
            self.run_frame();
        }
        moved
    }

    #[cfg(feature = "model")]
    /// Open the view nearest `resource` that the published resources offer (its own, else its
    /// closest parent's), as a person choosing it would.
    fn open_view_of(&mut self, resource: &str) -> bool {
        fn nearest(resources: &[tilo_actions::ResourceSpec], id: &str) -> Option<(String, String)> {
            resources.iter().find_map(|spec| {
                let own = || {
                    let view = spec
                        .views
                        .iter()
                        .find(|view| view.primary)
                        .or_else(|| spec.views.first())?;
                    Some((spec.id.as_str().to_owned(), view.open_action.clone()))
                };
                if spec.id.as_str() == id {
                    return own();
                }
                spec.children
                    .iter()
                    .any(|child| child.id.as_str() == id || nearest(&child.children, id).is_some())
                    .then(|| nearest(&spec.children, id).or_else(own))
                    .flatten()
            })
        }
        let Some((owner, open)) = nearest(&self.resources(), resource) else {
            return false;
        };
        let address = tilo_actions::ActionAddress::new(owner, open);
        self.invoke_action(&address, &serde_json::Map::new(), None)
            .is_ok()
    }
}

// Navigation, paging and the device's shell.
impl Driver {
    /// The system's back (or forward) gesture, as the host delivers it once it recognised one,
    /// then the frame that shows what it did. Whether something took it.
    pub fn nav_gesture(&mut self, dir: crate::gesture::NavDirection) -> bool {
        let taken = {
            let _running = self.running();
            self.app.surface_nav_gesture(dir)
        };
        self.run_frame();
        taken
    }

    /// How many items the app leaves out past each collection's published page, counted without
    /// building them ([`crate::widget::past_page_len`]).
    #[must_use]
    pub fn past_page_len(&self) -> usize {
        crate::widget::past_page_len(self.app.root_widget.as_ref(), &self.app.state.doc)
    }

    /// The items the app leaves out past each collection's published page from `offset` on, at
    /// most `take` of them, built alone ([`crate::widget::past_page_range`]).
    #[must_use]
    pub fn past_page_range(&self, offset: usize, take: usize) -> Vec<tilo_actions::ResourceSpec> {
        crate::widget::past_page_range(
            self.app.root_widget.as_ref(),
            &self.app.state.doc,
            offset,
            take,
        )
    }

    /// Deliver a trusted shell operation to the app, as its window's host does, then run the frame
    /// that shows it.
    ///
    /// # Errors
    /// When the app runs no shell, or the shell refuses the operation.
    pub fn device_shell_operation(
        &mut self,
        operation: &crate::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        let outcome = {
            let _running = self.running();
            crate::widget::capability::capability_mut::<dyn crate::AppWidget>(
                &mut *self.app.root_widget,
            )
            .ok_or_else(|| "Terminal is not available".to_owned())
            .and_then(|widget| widget.device_shell_operation(operation))
        };
        self.run_frame();
        outcome
    }

    /// Ask the app for the device's shell, as its host's trusted selection does, then run the
    /// frame that shows it. Whether the app took it.
    pub fn request_device_shell(&mut self) -> bool {
        let taken = {
            let _running = self.running();
            crate::widget::capability::capability_mut::<dyn crate::AppWidget>(
                &mut *self.app.root_widget,
            )
            .is_some_and(crate::AppWidget::request_device_shell)
        };
        self.run_frame();
        taken
    }
}
