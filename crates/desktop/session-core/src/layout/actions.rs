//! The layout's action API: what a palette, a script, an agent and the host's own calls
//! reach its actions through.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::Value as Json;
use tilo_actions::{Actor, MemberId, ResourceSpec};

use super::published::LayoutEffect;
use super::{
    Ask, AskOrigin, Busy, CanvasRect, Find, Found, HISTORY_RESOURCE, Layout, LayoutAction, Pointed,
    RESOURCE,
};
use tilo_resource::Effect as _;

/// The answer a layout action that asks the record gives: complete, with nothing for the
/// caller to read back. A place that stands on a later pass is complete when it is asked, never
/// left pending (`Accepted` would be, and nothing completes it).
fn completed() -> tilo_ui_base::host_operation::ResourceOperationStart {
    tilo_ui_base::host_operation::ResourceOperationStart::Completed(
        tilo_actions::ActionOutput::default(),
    )
}

/// The layout's published side: the one action API the palette, a script, an agent and the host's
/// own calls reach its actions through, beside the desktop's own.
#[derive(Clone)]
pub struct LayoutActions {
    pub(super) layout: Rc<RefCell<Layout>>,
    /// How the host says whether the person is mid-gesture or typing, as it last saw it.
    busy: Rc<dyn Fn() -> Busy>,
    /// The system's `find` , as the palette and an agent's `tilo` tool run it: what text typed on
    /// the canvas places (shell.md §8). Empty where the host has no index to search.
    find: Find,
    /// What the canvas asks of the host when the text it is given is a question, or names
    /// nothing (shell.md §8.1).
    ask: Ask,
    /// What the canvas asks of an app when the text names a type of its and names nothing the
    /// session holds: that app's own search action, through its origin (shell.md §8, search.md
    /// §7). Empty where the host can ask no app.
    ask_origin: AskOrigin,
}

impl LayoutActions {
    /// The action API over `layout` , whose changes wait while `busy` says the person is arranging
    /// or typing.
    #[must_use]
    pub fn new(
        layout: Rc<RefCell<Layout>>,
        busy: Rc<dyn Fn() -> Busy>,
        find: Find,
        ask: Ask,
        ask_origin: AskOrigin,
    ) -> Self {
        Self {
            layout,
            busy,
            find,
            ask,
            ask_origin,
        }
    }

    /// What the layout publishes now, as the host publishes it.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.layout.borrow().resources()
    }

    /// Carry out `address` with `arguments`.
    ///
    /// # Errors
    /// Why the action names nothing published, or why it could not be made.
    pub fn invoke(
        &self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<tilo_ui_base::host_operation::ResourceOperationStart, tilo_actions::ApiError> {
        self.invoke_by(None, address, arguments)
    }

    /// [`Self::invoke`] with the one who asks named: an actor that names no one is the person's own
    /// call.
    fn invoke_by(
        &self,
        actor: Option<&Actor>,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<tilo_ui_base::host_operation::ResourceOperationStart, tilo_actions::ApiError> {
        // The layout publishes its record's undo and redo beside its layout actions: both
        // step the record, never decode as a layout action.
        if address.resource == HISTORY_RESOURCE
            || (address.resource == RESOURCE && matches!(address.action.as_str(), "undo" | "redo"))
        {
            return self.step(address, arguments);
        }
        let published = self.layout.borrow().resources();
        let encoded = tilo_actions::api::encode_json(&published, address, arguments)?;
        let effect = LayoutEffect::from_payload(&encoded.payload)
            .map_err(|error| tilo_actions::ApiError::InvalidArguments(error.to_string()))?;
        // Clearing recents marks the person's own view of the layout, never the layout itself.
        if let LayoutEffect::ClearRecents { app, kind } = &effect {
            let scope = tilo_desktop_model::recents::Scope::of(app.as_deref(), kind.as_deref());
            if !self.layout.borrow().clear_recents(&scope) {
                return Err(tilo_actions::ApiError::Unavailable {
                    code: "recents_not_kept".to_owned(),
                    reason:
                        "the person's view of this workspace's layout cannot be written here yet"
                            .to_owned(),
                });
            }
            return Ok(completed());
        }
        let Some(mut action) = effect.layout() else {
            return self.step(address, arguments);
        };
        // An action that names no one is the person's own input: theirs to take back, and never
        // made to wait for themselves.
        let person = Actor::from(MemberId::person(self.person()));
        let asked = actor.unwrap_or(&person).clone();
        // What a `place`, a `show` or an `open` names no view of stands as content, through the
        // view its app declares for a place of it, wherever it ends up: beside a turn it
        // was aimed at too.
        self.place_as_content(&mut action);
        if self.kept_turn(&asked, arguments, &action)? {
            return Ok(self.shown(&action));
        }
        if address.action == "place" {
            if self.placed_beside(&asked, arguments, &action)? {
                return Ok(self.shown(&action));
            }
            self.place_at_surface(arguments, &mut action)?;
        }
        if let LayoutAction::Position { place, x, y } = &mut action {
            self.position_beside(arguments, *place, x, y)?;
        }
        // The person's own change is what their gesture is making; only anyone else's waits for
        // them to finish.
        let busy = if asked == person {
            Busy::default()
        } else {
            (self.busy)()
        };
        // A place found by text, an `open` and a retried search are resolved where what they read
        // is, never in the record: each answers here, or names none of itself and is asked below.
        if let Some(step) = self.found_by_text(&action, &asked, busy) {
            return step;
        }
        if let Some(step) = self.opened(&action, &asked) {
            return step;
        }
        if let Some(step) = self.retried(&action, &asked, busy) {
            return step;
        }
        self.layout
            .borrow_mut()
            .ask(&asked, action.clone(), busy)
            .map(drop)
            .map_err(tilo_actions::ApiError::Failed)?;
        Ok(self.shown(&action))
    }

    /// What a layout action that shows a resource answers once it ran: the title of the place
    /// showing it, as every surface captions it ("All inboxes", canvas.md §7 "Captions name the
    /// thing"), so whoever asked says what it showed by that name (an agent's step line). Any
    /// other action, or one whose place does not stand yet (it waits for the person's gesture),
    /// answers nothing.
    fn shown(&self, action: &LayoutAction) -> tilo_ui_base::host_operation::ResourceOperationStart {
        let (own, resource) = match action {
            LayoutAction::Place {
                place, resource, ..
            } => (Some(*place), resource),
            LayoutAction::Open { resource, .. } | LayoutAction::Show { resource, .. } => {
                (None, resource)
            }
            _ => return completed(),
        };
        let layout = self.layout.borrow();
        let places = layout.reading().places;
        let title = own
            .into_iter()
            .chain(layout.anchor_of(resource))
            .find_map(|place| {
                let key = place.to_string();
                places.iter().find(|held| held.key == key)
            })
            .map(|held| {
                if held.title.trim().is_empty() {
                    held.name.trim().to_owned()
                } else {
                    held.title.trim().to_owned()
                }
            })
            .filter(|title| !title.is_empty());
        let Some(title) = title else {
            return completed();
        };
        tilo_ui_base::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput {
                value: Some(serde_json::json!({ "title": title })),
                resources: Vec::new(),
            },
        )
    }

    /// Search for the text a place found by text names, and stand what it finds beside where the
    /// person typed: `find` answers the hits on whatever thread it needs, and `place_found` makes
    /// the place on a later pass, as the person's own step.
    fn find_and_place_found(
        &self,
        asked: &Actor,
        text: &str,
        view: &str,
        strip: Option<u64>,
        (at, size, seen): Pointed,
        busy: Busy,
    ) {
        let find = Rc::clone(&self.find);
        let ask = Rc::clone(&self.ask);
        let ask_origin = Rc::clone(&self.ask_origin);
        let api = self.clone();
        let needle = text.trim().to_owned();
        let (asked, text, view) = (asked.to_owned(), text.to_owned(), view.to_owned());
        find(
            &needle,
            Box::new(move |hits| {
                if let Err(why) = api.layout.borrow_mut().place_found(Found {
                    asked: &asked,
                    text: &text,
                    view: &view,
                    strip,
                    at,
                    size,
                    seen,
                    hits: &hits,
                    ask: &*ask,
                    ask_origin: &*ask_origin,
                    busy,
                }) {
                    // The text named nothing a place can show, and nothing answered it: the
                    // person sees no place, so the log says why.
                    tracing::warn!(%why, %text, "what the text found is not placed");
                }
            }),
        );
    }

    /// A place found by text is resolved where the search and what is published now are: the
    /// record only ever holds what was placed. The search runs first, on whatever thread it needs
    /// (a host whose sources read a disk, a mailbox or a service answers later), and this answers
    /// before the place stands: the place appears on a later pass, the person's step as any other.
    /// `None` for any other action.
    fn found_by_text(
        &self,
        action: &LayoutAction,
        asked: &Actor,
        busy: Busy,
    ) -> Option<Result<tilo_ui_base::host_operation::ResourceOperationStart, tilo_actions::ApiError>>
    {
        let LayoutAction::PlaceFound {
            text,
            view,
            strip,
            x,
            y,
            width,
            height,
            seen_x,
            seen_y,
            seen_width,
            seen_height,
        } = action
        else {
            return None;
        };
        // A rectangle the person dragged is its size; a click names none.
        let size = (*width > 0 && *height > 0).then_some((*width, *height));
        let seen = (*seen_width > 0 && *seen_height > 0).then_some(CanvasRect {
            x: *seen_x,
            y: *seen_y,
            width: *seen_width,
            height: *seen_height,
        });
        self.find_and_place_found(asked, text, view, *strip, ((*x, *y), size, seen), busy);
        // The search is under way and the place stands on a later pass: the API answers as it
        // always did for a layout action, which is complete when the ask is taken. Nothing of
        // the place is left for the caller to read back, so it is not a pending invocation
        // (`Accepted` would be, and nothing completes it).
        Some(Ok(completed()))
    }

    /// The system's `open` is resolved here, where the layout's ids and its places are: it becomes
    /// the `Place` it makes beside its anchor, or the `Activate` that brings what is open forward,
    /// and the record only ever holds ids. `None` for any other action.
    fn opened(
        &self,
        action: &LayoutAction,
        asked: &Actor,
    ) -> Option<Result<tilo_ui_base::host_operation::ResourceOperationStart, tilo_actions::ApiError>>
    {
        let LayoutAction::Open {
            resource,
            view,
            beside,
        } = action
        else {
            return None;
        };
        let opened = self
            .layout
            .borrow_mut()
            .open_place(asked, resource, view, beside);
        Some(match opened {
            Ok(_) => Ok(self.shown(action)),
            Err(why) => Err(tilo_actions::ApiError::Unavailable {
                code: "nowhere_to_open".to_owned(),
                reason: why,
            }),
        })
    }

    /// A query's Retry asks its app again, from the text it holds: the query goes back to
    /// searching, and the app's own search action runs the text once more through `ask_origin`
    /// (`Layout::retry_query`). Resolved here, where the app's action is, never in the
    /// record. `None` for any other action.
    fn retried(
        &self,
        action: &LayoutAction,
        asked: &Actor,
        busy: Busy,
    ) -> Option<Result<tilo_ui_base::host_operation::ResourceOperationStart, tilo_actions::ApiError>>
    {
        let LayoutAction::RetryQuery { place } = action else {
            return None;
        };
        let ask_origin = Rc::clone(&self.ask_origin);
        let retried = self
            .layout
            .borrow_mut()
            .retry_query(asked, *place, &*ask_origin, busy);
        Some(match retried {
            Ok(()) => Ok(completed()),
            Err(why) => Err(tilo_actions::ApiError::Unavailable {
                code: "nothing_to_place".to_owned(),
                reason: why,
            }),
        })
    }

    /// [`Self::invoke`] with `actor` as the one who asks: the name their step of the layout's history
    /// carries, and whose gesture an action that is not the person's waits for.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn invoke_as(
        &self,
        actor: &Actor,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<tilo_ui_base::host_operation::ResourceOperationStart, tilo_actions::ApiError> {
        self.invoke_by(Some(actor), address, arguments)
    }

    /// A `place`, a `show` or an `open` that names no view stands its resource as content, through
    /// the view its app declares for a place of it (`place view`), never through the app's whole
    /// window: an agent, the REPL and a reference's activation place the thing, as text typed on
    /// the canvas does (canvas.md §7, "The thing, never the app"). The app's window is
    /// `open-app`'s, or the view a caller names.
    fn place_as_content(&self, action: &mut LayoutAction) {
        let (LayoutAction::Place { resource, view, .. }
        | LayoutAction::Show { resource, view, .. }
        | LayoutAction::Open { resource, view, .. }) = action
        else {
            return;
        };
        if !view.trim().is_empty() {
            return;
        }
        let targets = self.layout.borrow().targets();
        let place_view = targets
            .borrow()
            .get(resource.trim())
            .and_then(|target| target.place_view.clone());
        if let Some(place_view) = place_view {
            *view = place_view;
        }
    }

    /// A `place` that names no workspace, row, strip or id of its own stands on the
    /// surface in front, in a strip of its own, under ids minted here: what a reference
    /// activated in a view asks for (shell.md §3). They go into the action itself,
    /// never back through the arguments: a layout's ids span all of `u64`, which an
    /// integer argument does not.
    fn place_at_surface(
        &self,
        arguments: &serde_json::Map<String, Json>,
        action: &mut LayoutAction,
    ) -> Result<(), tilo_actions::ApiError> {
        let LayoutAction::Place {
            workspace,
            row,
            strip,
            place,
            ..
        } = action
        else {
            return Ok(());
        };
        let named = ["workspace", "row", "strip", "place"]
            .iter()
            .all(|id| arguments.contains_key(*id));
        if named {
            return Ok(());
        }
        // A row is a row of one workspace: the two come named together, or both from the surface in
        // front.
        if arguments.contains_key("workspace") != arguments.contains_key("row") {
            return Err(tilo_actions::ApiError::InvalidArguments(
                "a place names its workspace and its row together, or neither".to_owned(),
            ));
        }
        let surface = self.layout.borrow().surface_at().map_err(|why| {
            tilo_actions::ApiError::Unavailable {
                code: "nowhere_to_place".to_owned(),
                reason: why,
            }
        })?;
        if !arguments.contains_key("workspace") {
            *workspace = surface.workspace;
            *row = surface.row;
        }
        if !arguments.contains_key("strip") {
            *strip = surface.strip;
        }
        if !arguments.contains_key("place") {
            *place = surface.place;
        }
        Ok(())
    }

    /// Undo or redo the person's last change of the record; an undo naming an `invocation` takes
    /// back every change of that invocation instead, and nothing left of it is no error.
    fn step(
        &self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<tilo_ui_base::host_operation::ResourceOperationStart, tilo_actions::ApiError> {
        if address.action == "undo"
            && let Some(invocation) = arguments.get("invocation").and_then(Json::as_str)
        {
            self.layout
                .borrow()
                .take_back(invocation)
                .map_err(tilo_actions::ApiError::Failed)?;
            return Ok(completed());
        }
        let person = self.person();
        let stepped = match address.action.as_str() {
            "undo" => self.layout.borrow_mut().undo(&person),
            "redo" => self.layout.borrow_mut().redo(&person),
            _ => return Err(tilo_actions::ApiError::Unknown(address.clone())),
        };
        if !stepped {
            return Err(tilo_actions::ApiError::Unavailable {
                code: "nothing_to_undo".to_owned(),
                reason: "the layout has no step left".to_owned(),
            });
        }
        Ok(completed())
    }

    /// The person whose layout it is.
    fn person(&self) -> String {
        self.layout.borrow().person().to_owned()
    }
}

impl crate::surfaces::Published for LayoutActions {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        Self::resources(self)
    }

    fn revision(&self) -> Option<u64> {
        Some(self.layout.borrow().revision())
    }

    fn invoke(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
        _look: &mut crate::surfaces::Look<'_>,
    ) -> Result<tilo_ui_base::host_operation::ResourceOperationStart, tilo_actions::ApiError> {
        Self::invoke(self, address, arguments)
    }

    fn invoke_as(
        &mut self,
        actor: &Actor,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
        _look: &mut crate::surfaces::Look<'_>,
    ) -> Result<tilo_ui_base::host_operation::ResourceOperationStart, tilo_actions::ApiError> {
        Self::invoke_as(self, actor, address, arguments)
    }

    /// The names an assistive technology reads for what it publishes: none of its own.
    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}
