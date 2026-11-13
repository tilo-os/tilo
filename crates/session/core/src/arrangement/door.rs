//! The arrangement's action door: what a palette, a script, an agent and the host's own calls
//! reach its actions through.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::Value as Json;
use tilo_actions::ResourceSpec;

use super::{
    Arrangement, ArrangementAction, Ask, AskOrigin, Busy, Find, Found, RECORD_RESOURCE, RESOURCE,
};

/// The arrangement's published side: the one door the palette, a script, an agent and the host's
/// own calls reach its actions through, beside the desktop's own.
#[derive(Clone)]
pub struct Door {
    arrangement: Rc<RefCell<Arrangement>>,
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

impl Door {
    /// The door over `arrangement` , whose changes wait while `busy` says the person is arranging
    /// or typing.
    #[must_use]
    pub fn new(
        arrangement: Rc<RefCell<Arrangement>>,
        busy: Rc<dyn Fn() -> Busy>,
        find: Find,
        ask: Ask,
        ask_origin: AskOrigin,
    ) -> Self {
        Self {
            arrangement,
            busy,
            find,
            ask,
            ask_origin,
        }
    }

    /// What the arrangement publishes now, as the host publishes it.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.arrangement.borrow().resources()
    }

    /// Where what it publishes stands ([`Arrangement::revision`]).
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.arrangement.borrow().revision()
    }

    /// The names an assistive technology reads for what it publishes: none of its own.
    #[must_use]
    pub fn accessible_labels(&self) -> Vec<String> {
        Vec::new()
    }

    /// Carry out `address` with `arguments`.
    ///
    /// # Errors
    /// Why the action names nothing published, or why it could not be made.
    pub fn invoke(
        &self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        self.invoke_by(None, address, arguments)
    }

    /// [`Self::invoke`] with the one who asks named: an actor that names no one is the person's own
    /// call.
    fn invoke_by(
        &self,
        actor: Option<&str>,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        // The arrangement publishes its record's undo and redo beside its arranging actions: both
        // step the record, never decode as an arranging action.
        if address.resource == RECORD_RESOURCE
            || (address.resource == RESOURCE && matches!(address.action.as_str(), "undo" | "redo"))
        {
            return self.step(address);
        }
        let published = self.arrangement.borrow().resources();
        let built = tilo_actions::door::spec_of(&published, &address.resource, &address.action)
            .ok_or_else(|| tilo_actions::DoorError::Unknown(address.clone()))?;
        let encoded = built
            .build_json(arguments)
            .map_err(|error| tilo_actions::DoorError::InvalidArguments(error.to_string()))?;
        let mut action =
            ArrangementAction::decode(&encoded.type_id, &encoded.payload).ok_or_else(|| {
                tilo_actions::DoorError::UnregisteredType("tilo.shell.arranging".to_owned())
            })?;
        if address.action == "place" {
            self.place_at_surface(arguments, &mut action)?;
        }
        // An action that names no one is the person's own input: theirs to take back, and never
        // made to wait for themselves.
        let asked = match actor.filter(|actor| !actor.is_empty()) {
            Some(actor) => actor.to_owned(),
            None => self.person(),
        };
        // The person's own change is what their gesture is making; only anyone else's waits for
        // them to finish.
        let busy = if asked == self.person() {
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
        self.arrangement
            .borrow_mut()
            .ask(&asked, action, busy)
            .map(drop)
            .map_err(tilo_actions::DoorError::Failed)?;
        Ok(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        ))
    }

    /// A place found by text is resolved where the search and what is published now are: the
    /// record only ever holds what was placed. The search runs first, on whatever thread it needs
    /// (a host whose sources read a disk, a mailbox or a service answers later), and this answers
    /// before the place stands: the place appears on a later pass, the person's step as any other.
    /// `None` for any other action.
    fn found_by_text(
        &self,
        action: &ArrangementAction,
        asked: &str,
        busy: Busy,
    ) -> Option<Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError>> {
        let ArrangementAction::PlaceFound {
            text,
            view,
            strip,
            x,
            y,
        } = action
        else {
            return None;
        };
        let spot = (*x, *y);
        let find = Rc::clone(&self.find);
        let ask = Rc::clone(&self.ask);
        let ask_origin = Rc::clone(&self.ask_origin);
        let door = self.clone();
        let needle = text.trim().to_owned();
        let (text, view, strip) = (text.clone(), view.clone(), *strip);
        let asked = asked.to_owned();
        find(
            &needle,
            Box::new(move |hits| {
                if let Err(why) = door.arrangement.borrow_mut().place_found(Found {
                    asked: &asked,
                    text: &text,
                    view: &view,
                    strip,
                    at: spot,
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
        // The search is under way and the place stands on a later pass: the door answers as it
        // always did for an arranging action, which is complete when the ask is taken. Nothing of
        // the place is left for the caller to read back, so it is not a pending invocation
        // (`Accepted` would be, and nothing completes it).
        Some(Ok(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }

    /// The system's `open` is resolved here, where the desk's ids and its places are: it becomes
    /// the `Place` it makes beside its anchor, or the `Activate` that brings what is open forward,
    /// and the record only ever holds ids. `None` for any other action.
    fn opened(
        &self,
        action: &ArrangementAction,
        asked: &str,
    ) -> Option<Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError>> {
        let ArrangementAction::Open {
            resource,
            view,
            beside,
        } = action
        else {
            return None;
        };
        let opened = self
            .arrangement
            .borrow_mut()
            .open_place(asked, resource, view, beside);
        Some(match opened {
            Ok(_) => Ok(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )),
            Err(why) => Err(tilo_actions::DoorError::Unavailable {
                code: "nowhere_to_open".to_owned(),
                reason: why,
            }),
        })
    }

    /// A query's Retry asks its app again, from the text it holds: the query goes back to
    /// searching, and the app's own search action runs the text once more through `ask_origin`
    /// (`Arrangement::retry_query`). Resolved here, where the app's action is, never in the
    /// record. `None` for any other action.
    fn retried(
        &self,
        action: &ArrangementAction,
        asked: &str,
        busy: Busy,
    ) -> Option<Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError>> {
        let ArrangementAction::RetryQuery { place } = action else {
            return None;
        };
        let ask_origin = Rc::clone(&self.ask_origin);
        let retried = self
            .arrangement
            .borrow_mut()
            .retry_query(asked, *place, &*ask_origin, busy);
        Some(match retried {
            Ok(()) => Ok(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )),
            Err(why) => Err(tilo_actions::DoorError::Unavailable {
                code: "nothing_to_place".to_owned(),
                reason: why,
            }),
        })
    }

    /// [`Self::invoke`] with `actor` as the one who asks: the name their step of the desk's history
    /// carries, and whose gesture an action that is not the person's waits for.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn invoke_as(
        &self,
        actor: &str,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        self.invoke_by(Some(actor), address, arguments)
    }

    /// A `place` that names no workspace, row, strip or id of its own stands on the
    /// surface in front, in a strip of its own, under ids minted here: what a reference
    /// activated in a view asks for (shell.md §3). They go into the action itself,
    /// never back through the arguments: a desk's ids span all of `u64`, which an
    /// integer argument does not.
    fn place_at_surface(
        &self,
        arguments: &serde_json::Map<String, Json>,
        action: &mut ArrangementAction,
    ) -> Result<(), tilo_actions::DoorError> {
        let ArrangementAction::Place {
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
            return Err(tilo_actions::DoorError::InvalidArguments(
                "a place names its workspace and its row together, or neither".to_owned(),
            ));
        }
        let surface = self.arrangement.borrow().surface_at().map_err(|why| {
            tilo_actions::DoorError::Unavailable {
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

    /// Undo or redo the person's last change of the record.
    fn step(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        let person = self.person();
        let stepped = match address.action.as_str() {
            "undo" => self.arrangement.borrow_mut().undo(&person),
            "redo" => self.arrangement.borrow_mut().redo(&person),
            _ => return Err(tilo_actions::DoorError::Unknown(address.clone())),
        };
        if !stepped {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "nothing_to_undo".to_owned(),
                reason: "the arrangement has no step left".to_owned(),
            });
        }
        Ok(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        ))
    }

    /// The person whose arrangement it is.
    fn person(&self) -> String {
        self.arrangement.borrow().person().to_owned()
    }
}
