//! The surfaces the host owns for a window's actions: the bar over its columns, the menu an
//! item's right-click, long press or Menu key opens, and the "…" beside the title an item's
//! page edits.
//!
//! A view says what a person can do with what it shows by declaring actions on the type and by
//! placing the typed affordances of the thing itself (a checkbox that completes, an entry that
//! adds, a drag); it never reads `actions(r)` back to build chrome. The host reads those
//! declarations where a view rendered them — the resource its boundary was opened on, and each
//! item a row, card or tree node stands for — and puts them where every window keeps them, in
//! the same order and shape: the primary action once in the window's bar, a command that
//! toggles a state beside it, the rest behind the bar's own door, and on an item its actions in
//! its menu. An action's declaration says what it is; where it stands is read off that: a
//! command that shows whether it is on (`on:`) belongs where the person sees that, and every
//! other stands in a menu. A view that places the actions of what it shows says their order and
//! which one leads.

use std::collections::BTreeMap;

use crate::core::Core;
use crate::error::RuntimeError;
use crate::exec::MANY_ACTION;
use crate::handles::ViewState;
use crate::host_ui::{KEEP_ACTION, SET_ACTION, THEN_ACTION, VIEW_STATE};
use crate::value::{ActionRef, RefValue, Value};
use crate::view::{BindTarget, NodeBuild};
use crate::view_state::StateKey;

/// A node whose `item` gets the host's menu: what a person right-clicks, long-presses, or picks
/// out of with the Menu key.
const MEMBERS: [&str; 3] = ["ui.ListItem", "ui.TreeViewNode", "ui.BoardCard"];
/// The row of a list: the one member that swipes, too.
const LIST_ITEM: &str = "ui.ListItem";
/// The window's shell: the columns whose one bar the host fills.
const SHELL: &str = "ui.NavShell";
/// The menu a view opens onto a resource, its commands filled here.
const MENU_BUTTON: &str = "ui.MenuButton";
/// The slot a menu's commands go in.
const ITEMS: &str = "items";
/// The control that edits a title as a page's large title.
const TEXT_INPUT: &str = "ui.TextInput";
/// A slot of a component, named by its `name` property.
const SLOT: &str = "slot";
/// A line of content: its children side by side, the room a `fill`ing one leaves the others.
const ROW: &str = "row";
/// How far apart a row's parts stand, and where a part shorter than the line stands in it.
const GAP: &str = "gap";
const ALIGN: &str = "align";
/// The room a node takes of its row: a weight, divided among the nodes that fill.
const FILL: &str = "fill";

/// The prop a view places the actions of what it shows with (`actions:`): its ordered list, each
/// entry the action's id, the run of commands the view puts it in and whether the view makes it
/// the primary one. The host reads it while it builds the surfaces and takes it off the node, so
/// the tree it sends carries only what the widget declares.
const PLACEMENT: &str = "actions";
/// Where each field of a placed action stands, as the checker lowers one.
const PLACED_ID: usize = 0;
const PLACED_GROUP: usize = 1;
const PLACED_PRIMARY: usize = 2;
/// The run of commands a menu draws what its view left over in.
const MORE_RUN: &str = "tilolang-more";
const MORE_RUN_WORDS: &str = "More";

const MENU: &str = "menu";
const SWIPE: &str = "swipe";
const OVERVIEW: &str = "overview";
const OVERVIEW_ACTIONS: &str = "overview_actions";
const ACTIONS: &str = "actions";

/// The word a "…" is named by: the one the shell gives its own More button.
const MORE: &str = "window-toolbar-more";

/// The list whose rows a person chooses more than one of: the one that keeps the choice where
/// the window's bar reads it.
const MULTI_SELECT: &str = "multi_select";
/// The property a multi-selecting list is given back: the items of the person's choice, so the
/// list it builds again shows them chosen.
const CHOSEN: &str = "chosen";
/// The event a multi-selecting list reports its choice with.
const SELECTION_EVENT: &str = "selected";
/// The window slot holding what the person chose of what the window shows. It is the window's
/// own reading, kept for the person looking alone ([`StateKey::runtime`], set by the host), and
/// what the bar acts on: a selection belongs to where the person is, not to the workspace.
const SELECTION: &str = "ui:selection";
/// The words the selection's count reads with, and the way out of it.
const SELECTED: &str = "tilolang-selected";
const CLEAR_WORDS: (&str, &str) = ("tilolang-clear-selection", "Clear selection");
/// The id of the command that leaves a selection, as a surface offers it.
const CLEAR_ACTION: &str = "ui:clear-selection";

/// What a switch names the view labelled `full` of the type labelled `ty`: the view's label
/// with the type's name left off, so a Project's views read "List" and "Board" under a strip
/// the type already names. The full label stands when the type's name is not part of it.
fn switch_label(full: &str, ty: Option<&str>) -> String {
    let Some(rest) = ty.and_then(|ty| full.strip_prefix(ty)).map(str::trim) else {
        return full.to_owned();
    };
    let mut chars = rest.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => full.to_owned(),
    }
}

/// Where each field of a `host.Action` stands, as [`crate::platform`] offers it.
const LABEL: usize = 0;
const ICON: usize = 1;
const PRIMARY: usize = 3;
const DESTRUCTIVE: usize = 4;
const ON: usize = 5;
const KEY: usize = 6;
const RUN: usize = 8;
const GROUP: usize = 9;
const AVAILABLE: usize = 10;
const REASON: usize = 11;
const APPLIES: usize = 12;
const ID: usize = 13;

impl Core {
    /// The host's surfaces for the tree a boundary rendered: each item's menu, and the window's
    /// own bar, or for the page of one item the "…" beside its title. `args` are the boundary's,
    /// so the window's subject is the resource the view was opened on.
    pub(crate) fn surfaces(
        &self,
        root: usize,
        args: &[Value],
        shown: &str,
    ) -> Result<(), RuntimeError> {
        let subject = match args.first() {
            Some(Value::Ref(subject)) => Some(subject.clone()),
            _ => None,
        };
        self.item_surfaces(root)?;
        self.subject_menus(root)?;
        self.list_selections(root)?;
        // The window's shell, wherever the view's own frames put it: a window that draws itself
        // through `std.page` or `std.list` keeps its shell one boundary down. What the view
        // placed in its bar is read here, before the tree is sent: it is the host's, not the
        // shell's.
        let shell = self.find_shell(root)?;
        let placed = match shell {
            Some(shell) => self.take_placement(shell)?,
            None => None,
        };
        let Some(subject) = subject else {
            return Ok(());
        };
        let Some(shell) = shell else {
            return Ok(());
        };
        // Only what the window's own place offers: an action that reads a row over it has
        // none on a window's page.
        let acted: Vec<Value> = self
            .actions_on(&subject, &Value::none())
            .map_err(trapped)?
            .into_iter()
            .filter(|action| flag(action, APPLIES))
            .collect();
        self.costs
            .publish(u64::try_from(acted.len()).unwrap_or(u64::MAX));
        let listed = self
            .props(shell)?
            .get("content_kind")
            .and_then(Value::as_text)
            .is_some_and(|kind| kind == "list");
        let overview = self.slot(shell, OVERVIEW)?.is_some();
        // A choice of more than one item is the window's own while it holds: the bar offers
        // what the items offer, once over all of them, and nothing of the window's own subject
        // takes its place until the choice is put away.
        let chosen = self.chosen_items()?;
        if chosen.len() > 1 {
            let offered = self.selection_actions(&chosen)?;
            if overview {
                self.fill_selection_bar(shell, OVERVIEW_ACTIONS, &offered, chosen.len())?;
            } else if self.has_title(shell, &subject) {
                self.put_under_title(shell, &subject, &offered, None, None)?;
            } else {
                self.fill_selection_bar(shell, ACTIONS, &offered, chosen.len())?;
            }
            return Ok(());
        }
        let entry = self.declare_entry_key(shell, &acted)?;
        if overview {
            if let Some(switch) = self.view_switch(&subject, shown)? {
                self.put_at_in_slot(shell, OVERVIEW_ACTIONS, switch)?;
            }
            self.fill_bar(
                shell,
                OVERVIEW_ACTIONS,
                listed,
                &acted,
                entry.as_deref(),
                placed.as_deref(),
            )?;
        } else if self.has_title(shell, &subject) {
            let switch = self.view_switch(&subject, shown)?;
            self.put_under_title(shell, &subject, &acted, switch, placed.as_deref())?;
        } else {
            if let Some(switch) = self.view_switch(&subject, shown)? {
                self.put_at_in_slot(shell, ACTIONS, switch)?;
            }
            self.fill_bar(
                shell,
                ACTIONS,
                listed,
                &acted,
                entry.as_deref(),
                placed.as_deref(),
            )?;
        }
        Ok(())
    }

    /// Each list a person may choose more than one row of is given the choice they hold and
    /// reports every change of a multi-selection back: the choice is the window's, so the bar
    /// over another boundary reads it, and the list it builds again shows it.
    fn list_selections(&self, at: usize) -> Result<(), RuntimeError> {
        let props = self.props(at)?;
        if crate::view::is_virtual(&self.kind(at)?) && flag_prop(&props, MULTI_SELECT) {
            let key = self.selection_key()?;
            let (held, generation) = {
                let states = self.view_states.borrow();
                (states.get(&key).cloned(), states.generation(&key))
            };
            // A list reads the choice even before there is one: the moment the person makes
            // one, the list builds again showing it.
            let chosen = held.unwrap_or_else(|| Value::List(Vec::new()));
            self.note_state_read(key, generation, &chosen);
            self.put_prop(at, CHOSEN, chosen)?;
            let report = ActionRef::new(KEEP_ACTION, vec![Value::text(SELECTION), Value::Event]);
            self.put_event(at, SELECTION_EVENT, Value::Action(report))?;
        }
        for child in self.children(at)? {
            self.list_selections(child)?;
        }
        Ok(())
    }

    /// The window slot the person's choice is kept in, as the runtime reads it: their window's,
    /// their own.
    fn selection_key(&self) -> Result<StateKey, RuntimeError> {
        let (window, viewer) = self.window_here()?;
        Ok(StateKey::runtime(window, Some(viewer), SELECTION))
    }

    /// The window whose surfaces are being worked out, and the person looking.
    fn window_here(&self) -> Result<(String, String), RuntimeError> {
        self.with_view(|vs| Ok((vs.window.clone(), vs.viewer.0.clone())))
            .map_err(trapped)
    }

    /// The items the person chose of what the window shows, in the order they stand. Reading
    /// it is what makes the bar run again when the choice moves.
    fn chosen_items(&self) -> Result<Vec<RefValue>, RuntimeError> {
        let key = self.selection_key()?;
        let (held, generation) = {
            let states = self.view_states.borrow();
            (states.get(&key).cloned(), states.generation(&key))
        };
        // Read even before there is a choice: the bar runs again when the person makes one.
        let seen = held.clone().unwrap_or_else(|| Value::List(Vec::new()));
        self.note_state_read(key, generation, &seen);
        let Some(held) = held else {
            return Ok(Vec::new());
        };
        Ok(match held {
            Value::List(items) => items
                .into_iter()
                .filter_map(|item| match item {
                    Value::Ref(item) => Some(item),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        })
    }

    /// What is offered for `items` as one command each: the actions their own menus offer that
    /// apply to every one of them, in the order the first item's are. Running one applies it
    /// to all of them as one change (`$many`), and puts the choice away after it; the ways out
    /// of a choice stand last, so a person leaves it without the keyboard.
    fn selection_actions(&self, items: &[RefValue]) -> Result<Vec<Value>, RuntimeError> {
        let mut offered: Vec<Vec<Value>> = Vec::with_capacity(items.len());
        for item in items {
            let acted: Vec<Value> = self
                .actions_on(item, &Value::none())
                .map_err(trapped)?
                .into_iter()
                .filter(|action| flag(action, APPLIES))
                .collect();
            offered.push(acted);
        }
        let Some(first) = offered.first() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for action in first {
            let Some(id) = opt_text(action, ID) else {
                continue;
            };
            let mut runs = Vec::with_capacity(offered.len());
            let mut every_item_takes_it = true;
            let mut every_on = true;
            let mut all_on = true;
            for held in &offered {
                let Some(found) = held.iter().find(|held| opt_text(held, ID) == Some(id)) else {
                    every_item_takes_it = false;
                    break;
                };
                // What one item cannot have done to it is no command over all of them: a
                // command disabled for one of six items is not offered at all.
                if !flag(found, AVAILABLE) {
                    every_item_takes_it = false;
                    break;
                }
                match opt_flag(found, ON) {
                    Some(on) => all_on = all_on && on,
                    None => every_on = false,
                }
                if let Some(Value::Action(run)) = action_field(found, RUN) {
                    runs.push(Value::Action(run.clone()));
                }
            }
            if !every_item_takes_it || runs.len() != offered.len() {
                continue;
            }
            let mut shown = action.clone();
            Self::put_field(&mut shown, PRIMARY, Value::Bool(false));
            Self::put_field(&mut shown, GROUP, Value::none());
            Self::put_field(&mut shown, AVAILABLE, Value::Bool(true));
            Self::put_field(&mut shown, REASON, Value::text(""));
            if every_on {
                Self::put_field(&mut shown, ON, Value::some(Value::Bool(all_on)));
            }
            Self::put_field(&mut shown, RUN, Value::Action(Self::over_a_choice(runs)));
            out.push(shown);
        }
        let mut label = self.words.borrow().word(CLEAR_WORDS.0, &[], CLEAR_WORDS.1);
        if label.is_empty() {
            CLEAR_WORDS.1.clone_into(&mut label);
        }
        out.push(Value::Tuple(vec![
            Value::text(&label),
            Value::some(Value::text("x")),
            Value::text(""),
            Value::Bool(false),
            Value::Bool(false),
            Value::none(),
            Value::some(Value::text("Escape")),
            Value::List(Vec::new()),
            Value::Action(ActionRef::new(
                KEEP_ACTION,
                vec![Value::text(SELECTION), Value::List(Vec::new())],
            )),
            Value::none(),
            Value::Bool(true),
            Value::text(""),
            Value::Bool(true),
            Value::text(CLEAR_ACTION),
        ]));
        Ok(out)
    }

    /// `runs` as one change: an action applied to many items (`$many`), the choice put away
    /// once it is done.
    fn over_a_choice(runs: Vec<Value>) -> ActionRef {
        let many = ActionRef::new(MANY_ACTION, vec![Value::List(runs)]);
        let clear = ActionRef::new(
            KEEP_ACTION,
            vec![Value::text(SELECTION), Value::List(Vec::new())],
        );
        ActionRef::new(THEN_ACTION, vec![Value::Action(many), Value::Action(clear)])
    }

    /// Set the field `at` of an action tuple.
    fn put_field(action: &mut Value, at: usize, value: Value) {
        if let Value::Tuple(fields) = action
            && let Some(field) = fields.get_mut(at)
        {
            *field = value;
        }
    }

    /// The window's shell in the tree: the first one met, a view's own frame included.
    fn find_shell(&self, at: usize) -> Result<Option<usize>, RuntimeError> {
        if self.kind(at)? == SHELL {
            return Ok(Some(at));
        }
        for child in self.children(at)? {
            if let Some(found) = self.find_shell(child)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// Each node standing for an item gets that item's actions in its menu, and the row's
    /// destructive action (or its first) as its swipe: what a view attached to it stays first.
    ///
    /// An action that reads a place gets the item the view says it shows over the row
    /// (`above`), which a row of a list or a node of an outline states because only the view
    /// knows what it shows: a row's own frame holds no other row. A first of its group states
    /// none, so a move that needs a row over it is not offered there.
    fn item_surfaces(&self, at: usize) -> Result<(), RuntimeError> {
        let kind = self.kind(at)?;
        if MEMBERS.contains(&kind.as_str()) {
            let placed = self.take_placement(at)?;
            let props = self.props(at)?;
            if let Some(item) = ref_prop(&props, "item") {
                let above = ref_prop(&props, "above")
                    .map_or_else(Value::none, |over| Value::some(Value::Ref(over)));
                self.item_actions(at, &kind, &item, &above, placed.as_deref())?;
            }
        }
        for child in self.children(at)? {
            self.item_surfaces(child)?;
        }
        Ok(())
    }

    /// The actions of `item` on the node `at` of `kind`: each in its menu, and the row's
    /// destructive one (or its first) as its swipe, offered where `above` stands over it. A view
    /// that placed them (`actions:`) has the ones it named first, in its order, and the rest
    /// behind its own "More"; without a placement, each stands as its type offers it.
    fn item_actions(
        &self,
        at: usize,
        kind: &str,
        item: &RefValue,
        above: &Value,
        placed: Option<&Placed>,
    ) -> Result<(), RuntimeError> {
        let taken: Vec<Value> = self
            .actions_on(item, above)
            .map_err(trapped)?
            .into_iter()
            .filter(|action| flag(action, APPLIES))
            .collect();
        if taken.is_empty() {
            return Ok(());
        }
        // What publishing the item's offers costs a profile: the actions its menu shows.
        self.costs
            .publish(u64::try_from(taken.len()).unwrap_or(u64::MAX));
        let (shown, rest) = Self::as_placed(taken, placed);
        // A swipe is a placement too: a view that placed its actions gets one of those it
        // placed, and none while it placed none.
        let direct = shown.len();
        let key = self.key(at)?;
        let mut menu = shown;
        for action in rest {
            menu.push(self.behind_more(action));
        }
        for (index, action) in menu.iter().enumerate() {
            let item = Self::menu_item(action, &format!("{key}~action-{index}"));
            self.put_in_slot(at, MENU, item)?;
        }
        let swipeable = if placed.is_some() {
            menu.get(..direct).unwrap_or_default()
        } else {
            &menu[..]
        };
        if kind == LIST_ITEM && self.slot(at, SWIPE)?.is_none() {
            let swipe = swipeable
                .iter()
                .filter(|action| flag(action, AVAILABLE))
                .find(|action| flag(action, DESTRUCTIVE))
                .or_else(|| menu.iter().find(|action| flag(action, AVAILABLE)));
            if let Some(action) = swipe {
                let node = Self::swipe_action(action, &format!("{key}~swipe"));
                self.put_in_slot(at, SWIPE, node)?;
            }
        }
        Ok(())
    }

    /// `offered` as the view placed it: the actions it named, in its order, each with the group
    /// and the primacy it gave it; then the rest, which a menu draws behind its "More". Without
    /// a placement, every action stands as offered and nothing is left over.
    fn as_placed(offered: Vec<Value>, placed: Option<&Placed>) -> (Vec<Value>, Vec<Value>) {
        let Some(placed) = placed else {
            return (offered, Vec::new());
        };
        let mut left: Vec<Option<Value>> = offered.into_iter().map(Some).collect();
        let mut shown = Vec::with_capacity(placed.len());
        for (id, group, primary) in placed {
            let Some(index) = left.iter().position(|action| {
                action
                    .as_ref()
                    .is_some_and(|action| opt_text(action, ID) == Some(id.as_str()))
            }) else {
                continue;
            };
            let Some(mut action) = left.get_mut(index).and_then(Option::take) else {
                continue;
            };
            if *primary {
                Self::put_field(&mut action, PRIMARY, Value::Bool(true));
            }
            // A view that places an action says which run of commands it stands in: the
            // placement is the menu, and the action's own run is what its type says where no
            // view placed it.
            Self::put_field(
                &mut action,
                GROUP,
                group
                    .as_ref()
                    .map_or_else(Value::none, |group| Value::some(Value::text(group))),
            );
            shown.push(action);
        }
        let rest = left.into_iter().flatten().collect();
        (shown, rest)
    }

    /// `action` as one a menu draws behind its "More": in that run, and no longer the primary
    /// one of a bar.
    fn behind_more(&self, mut action: Value) -> Value {
        let more = self.words.borrow().word(MORE_RUN, &[], MORE_RUN_WORDS);
        Self::put_field(&mut action, PRIMARY, Value::Bool(false));
        Self::put_field(&mut action, GROUP, Value::some(Value::text(&more)));
        action
    }

    /// What the view placed on the node `at`, taken off it: the host reads the placement while
    /// it builds the surfaces, and the tree it sends carries only what the widget declares.
    fn take_placement(&self, at: usize) -> Result<Option<Vec<PlacedAction>>, RuntimeError> {
        let props = self.props(at)?;
        let placed = placement(&props);
        if placed.is_some() {
            self.remove_prop(at, PLACEMENT)?;
        }
        Ok(placed)
    }

    /// Each menu a view opened onto a resource gets that resource's actions: a bar naming the
    /// window in front draws the commands the window's own menu would, without the view reading
    /// them back.
    fn subject_menus(&self, at: usize) -> Result<(), RuntimeError> {
        let subject = if self.kind(at)? == MENU_BUTTON {
            match self.props(at)?.get("subject").cloned() {
                Some(Value::Ref(subject)) => Some(subject),
                Some(Value::Opt(Some(held))) => match *held {
                    Value::Ref(subject) => Some(subject),
                    _ => None,
                },
                _ => None,
            }
        } else {
            None
        };
        if let Some(subject) = subject {
            let acted = self
                .actions_on(&subject, &Value::none())
                .map_err(trapped)?
                .into_iter()
                .filter(|action| flag(action, APPLIES));
            let key = self.key(at)?;
            let mut published = 0u64;
            for (index, action) in acted.enumerate() {
                let item = Self::menu_item(&action, &format!("{key}~item-{index}"));
                self.put_in_slot(at, ITEMS, item)?;
                published = published.saturating_add(1);
            }
            self.costs.publish(published);
        }
        self.each_child(at, |child| self.subject_menus(child))
    }

    /// Visit each child of `at`, in order.
    fn each_child(
        &self,
        at: usize,
        mut visit: impl FnMut(usize) -> Result<(), RuntimeError>,
    ) -> Result<(), RuntimeError> {
        for child in self.children(at)? {
            visit(child)?;
        }
        Ok(())
    }

    /// The switch over the views of the resource the window shows, when its type offers more
    /// than one view that draws it whole: a tab per view, the one the window shows chosen, and a
    /// press choosing another for the window, which [`render`](crate::render) reads where it
    /// draws the resource. `None` for a type with one view, which stands as the window itself.
    ///
    /// `shown` is the view the window draws the resource through now, so the switch shows what
    /// is on screen whatever chose it. The type's name names the strip, and each tab says only
    /// what the view adds to it: a Project's views read "List" and "Board".
    fn view_switch(&self, subject: &RefValue, shown: &str) -> Result<Option<usize>, RuntimeError> {
        let views = { self.registry.borrow().views_of_type(&subject.target.ty) };
        if views.len() < 2 {
            return Ok(None);
        }
        let (window, viewer) = self
            .with_view(|vs| Ok((vs.window.clone(), vs.viewer.0.clone())))
            .map_err(trapped)?;
        let state = format!("{VIEW_STATE}{}", subject.target.id_string());
        let chosen = self.chosen_index(&window, &viewer, &state, &subject.target.ty, &views, shown);
        let base = format!("{window}~views");
        let mut tabs = empty("ui.ViewTabs", &base);
        tabs.props.insert(
            "selected".to_owned(),
            Value::Opt(Some(Box::new(Value::Int(chosen)))),
        );
        let type_label = self.type_label(&subject.target.ty);
        if let Some(label) = &type_label {
            tabs.props.insert("label".to_owned(), Value::text(label));
        }
        tabs.events.insert(
            "change".to_owned(),
            Value::Action(ActionRef::new(
                SET_ACTION,
                vec![
                    Value::text(&window),
                    Value::text(&viewer),
                    Value::text(&state),
                    Value::Event,
                ],
            )),
        );
        let at = self.push(tabs)?;
        for (index, (id, name)) in views.iter().enumerate() {
            let (label, icon) = {
                let registry = self.registry.borrow();
                registry.view_shown(&subject.target.ty, id)
            };
            let full = self.words.borrow().word(
                &tilolang::messages::key_of(id),
                &[],
                &tilolang::messages::label(name),
            );
            let label = label.unwrap_or_else(|| switch_label(&full, type_label.as_deref()));
            let mut segment = empty("ui.Segment", &format!("{base}~view-{index}"));
            segment
                .props
                .insert("label".to_owned(), Value::text(&label));
            if let Some(icon) = icon {
                segment.props.insert("icon".to_owned(), Value::text(&icon));
            }
            let child = self.push(segment)?;
            self.insert(at, usize::MAX, child)?;
        }
        Ok(Some(at))
    }

    /// Which of `views` the person's window shows `ty`'s resource through: the view it draws
    /// the resource through now, else the person's choice, else the type's default view, else
    /// the first. The choice is read from the person's view state, so the bar runs again
    /// when another tab is chosen.
    fn chosen_index(
        &self,
        window: &str,
        viewer: &str,
        state: &str,
        ty: &str,
        views: &[(String, String)],
        shown: &str,
    ) -> i64 {
        // What the window draws now comes first: a window opened straight onto a view shows
        // that view chosen, before any choice or default of its own.
        if let Some(index) = views.iter().position(|(id, _)| id == shown) {
            return i64::try_from(index).unwrap_or_default();
        }
        let key = StateKey::view(window, viewer, state.to_owned());
        let (held, generation) = {
            let states = self.view_states.borrow();
            (states.get(&key).cloned(), states.generation(&key))
        };
        let seen = held.clone().unwrap_or_else(Value::none);
        self.note_state_read(key, generation, &seen);
        if let Some(Value::Int(index)) = held
            && let Ok(index) = usize::try_from(index)
            && index < views.len()
        {
            return i64::try_from(index).unwrap_or_default();
        }
        let default = { self.registry.borrow().default_view(ty) };
        if let Some(default) = default
            && let Some(index) = views.iter().position(|(id, _)| *id == default)
        {
            return i64::try_from(index).unwrap_or_default();
        }
        0
    }

    /// What the type `ty` is called, in the app's words: the name of the group of views the
    /// switch chooses between.
    fn type_label(&self, ty: &str) -> Option<String> {
        let (id, name) = {
            let registry = self.registry.borrow();
            let (_, info) = registry.type_info(ty)?;
            (info.id.clone(), info.name.clone())
        };
        Some(self.words.borrow().word(
            &tilolang::messages::key_of(&id),
            &[],
            &tilolang::messages::label(&name),
        ))
    }

    /// The window's bar's controls: what the shell searches with, then the primary action, the
    /// commands that toggle a state of the window's subject, and the rest where the bar's own
    /// door lists them. A view that placed the window's actions (`actions:`) has the ones it
    /// named in its bar, in its order, and the rest behind a "More" the bar keeps itself.
    fn fill_bar(
        &self,
        shell: usize,
        slot: &str,
        listed: bool,
        acted: &[Value],
        entry: Option<&str>,
        placed: Option<&Placed>,
    ) -> Result<(), RuntimeError> {
        let key = self.key(shell)?;
        let (acted, rest) = Self::as_placed(acted.to_vec(), placed);
        let mut by_place: [Vec<Value>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for action in acted {
            let place = if flag(&action, PRIMARY) {
                0
            } else if opt_flag(&action, ON).is_some() {
                1
            } else {
                2
            };
            if let Some(actions) = by_place.get_mut(place) {
                actions.push(action);
            }
        }
        if listed || slot == OVERVIEW_ACTIONS {
            self.put_in_slot(
                shell,
                slot,
                empty("ui.SearchButton", &format!("{key}~{slot}~search")),
            )?;
        }
        for (index, action) in by_place.into_iter().flatten().enumerate() {
            let primary = flag(&action, PRIMARY);
            let node = Self::action_button(
                &action,
                &format!("{key}~{slot}~action-{index}"),
                entry,
                true,
            );
            // The window's main action leads the actions of its bar: it stands where a person
            // looks first, and the app's own controls follow it.
            if primary {
                self.put_in_slot_front(shell, slot, node)?;
            } else {
                self.put_in_slot(shell, slot, node)?;
            }
        }
        // What the view left unplaced stands in the bar's own "…", beside what does not fit in
        // its row: one bar has one door, and the app's own menu beside it is a second "…" with
        // nothing to tell them apart.
        for (index, action) in rest.iter().enumerate() {
            let mut node =
                Self::action_button(action, &format!("{key}~{slot}~more-{index}"), entry, false);
            node.props.insert("in_menu".to_owned(), on_off(true));
            self.put_in_slot(shell, slot, node)?;
        }
        self.costs
            .publish(u64::try_from(rest.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    /// The button a bar keeps what its view left over behind: the shell's own "More", which
    /// lists what it holds.
    fn more_menu(&self, key: &str) -> NodeBuild {
        let mut menu = empty("ui.MenuButton", key);
        let label = self.words.borrow().word(MORE, &[], "More actions");
        menu.props.insert("label".to_owned(), Value::text(&label));
        menu.props
            .insert("icon".to_owned(), Value::text("more-horizontal"));
        menu.props.insert("icon_only".to_owned(), on_off(true));
        menu
    }

    /// The window's bar while a choice of many items holds: how many items the person chose,
    /// then what can be done with them once. Every command stands labelled, in the order the
    /// items' own actions are offered; the shell's own layout folds the ones that do not fit
    /// into its door. Nothing of the window's own subject stands beside them: the choice is
    /// what the person is working on.
    fn fill_selection_bar(
        &self,
        shell: usize,
        slot: &str,
        acted: &[Value],
        count: usize,
    ) -> Result<(), RuntimeError> {
        let key = self.key(shell)?;
        // The count reads in the reader's language, and as the count when it has no word for
        // this store's words.
        let label = self.words.borrow().word(
            SELECTED,
            &[("count".to_owned(), count.to_string())],
            &format!("{count} selected"),
        );
        let mut badge = empty("ui.Badge", &format!("{key}~{slot}~selected"));
        badge.props.insert("text".to_owned(), Value::text(&label));
        self.put_in_slot(shell, slot, badge)?;
        for (index, action) in acted.iter().enumerate() {
            let node =
                Self::action_button(action, &format!("{key}~{slot}~action-{index}"), None, false);
            self.put_in_slot(shell, slot, node)?;
        }
        Ok(())
    }

    /// The entry at a list's foot declares the window's primary action's key, so that the key
    /// and the bar's Primary bring the person there to type the item, rather than making one
    /// nobody typed: the entry is its own draft, and the item exists once it is committed. The
    /// key the entry declares is the answer. A key the field declares itself stands.
    fn declare_entry_key(
        &self,
        at: usize,
        acted: &[Value],
    ) -> Result<Option<String>, RuntimeError> {
        let Some(primary) = acted.iter().find(|action| flag(action, PRIMARY)) else {
            return Ok(None);
        };
        let Some(shortcut) = opt_text(primary, KEY) else {
            return Ok(None);
        };
        if self.put_entry_key(at, shortcut)? {
            return Ok(Some(shortcut.to_owned()));
        }
        Ok(None)
    }

    /// Give the entry at `at` or under it the key `shortcut`, unless a key already stands there.
    /// Whether there is such an entry.
    fn put_entry_key(&self, at: usize, shortcut: &str) -> Result<bool, RuntimeError> {
        let props = self.props(at)?;
        let entry = self.kind(at)? == TEXT_INPUT && flag_prop(&props, "entry");
        if entry {
            if !props.contains_key("key") {
                self.put_prop(at, "key", Value::text(shortcut))?;
            }
            return Ok(true);
        }
        for child in self.children(at)? {
            if self.put_entry_key(child, shortcut)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the content under `shell` edits the title of `subject` as its large title.
    fn has_title(&self, shell: usize, subject: &RefValue) -> bool {
        self.finds_title(shell, &subject.target.id_string())
            .is_some()
    }

    /// The "…" beside the title: the item's actions, the ones its context menu holds; the view
    /// switch `switch` before it, for a resource whose type offers more than one view. An item
    /// that offers neither shows no "…": a door opening on nothing only takes a line of the
    /// page. One that offers either keeps it at the trailing edge of the title's line, which is
    /// made for it when the view left the title in a column of its own. A view that placed the
    /// actions (`actions:`) has the ones it named beside the title, in its order, and the rest
    /// behind the "…".
    fn put_under_title(
        &self,
        shell: usize,
        subject: &RefValue,
        acted: &[Value],
        switch: Option<usize>,
        placed: Option<&Placed>,
    ) -> Result<(), RuntimeError> {
        if acted.is_empty() && switch.is_none() {
            return Ok(());
        }
        let Some((parent, index, key)) = self.finds_title(shell, &subject.target.id_string())
        else {
            return Ok(());
        };
        let (line, after) = self.title_line(parent, index, &key)?;
        // Without a placement every action stands behind the door, as the types offer them; a
        // view that placed some has those beside the title and the rest behind the door.
        let (shown, rest) = if placed.is_some() {
            Self::as_placed(acted.to_vec(), placed)
        } else {
            (Vec::new(), acted.to_vec())
        };
        let mut beside = after;
        for (index, action) in shown.iter().enumerate() {
            let node = Self::action_button(action, &format!("{key}~more-bar-{index}"), None, true);
            let child = self.push(node)?;
            self.insert(line, beside, child)?;
            beside = beside.saturating_add(1);
        }
        // A view that placed every action it offers leaves nothing for the door: it opens on
        // nothing, so it does not stand there.
        if !rest.is_empty() {
            let menu = self.more_menu(&format!("{key}~more"));
            let at = self.push(menu)?;
            for (index, action) in rest.iter().enumerate() {
                let item = Self::menu_item(action, &format!("{key}~more-{index}"));
                let child = self.push(item)?;
                self.insert(at, usize::MAX, child)?;
            }
            self.insert(line, beside, at)?;
            beside = beside.saturating_add(1);
        }
        if let Some(switch) = switch {
            self.insert(line, beside, switch)?;
        }
        Ok(())
    }

    /// The line the title `index` of `parent` stands on, and the place beside it: `parent`
    /// itself when it lays its children in a row, else a row made for the title, which takes its
    /// place among `parent`'s children with the title in it. A title spans the line it is on, so
    /// what stands beside it stands at the line's end.
    fn title_line(
        &self,
        parent: usize,
        index: usize,
        key: &str,
    ) -> Result<(usize, usize), RuntimeError> {
        if self.kind(parent)? == ROW {
            return Ok((parent, index.saturating_add(1)));
        }
        let title = self.arena(|view| {
            view.nodes
                .get(parent)
                .and_then(|node| node.children.get(index).copied())
                .ok_or_else(|| "unknown ui node".to_owned())
        })?;
        if !self.props(title)?.contains_key(FILL) {
            self.put_prop(title, FILL, Value::Float(1.0))?;
        }
        let mut line = empty(ROW, &format!("{key}~line"));
        line.props.insert(GAP.to_owned(), Value::text("related"));
        line.props.insert(ALIGN.to_owned(), Value::text("start"));
        let line = self.push(line)?;
        self.insert(line, 0, title)?;
        self.remove(parent, index)?;
        self.insert(parent, index, line)?;
        Ok((line, 1))
    }

    /// The control under `shell` that edits `subject`'s title: its parent, its place among the
    /// parent's children, and its key.
    fn finds_title(&self, at: usize, resource: &str) -> Option<(usize, usize, String)> {
        for (index, child) in self.children(at).ok()?.into_iter().enumerate() {
            if let Some(found) = self.finds_title(child, resource) {
                return Some(found);
            }
            let kind = self.kind(child).ok()?;
            if kind == TEXT_INPUT
                && self.edits_title(child, resource)
                && let Ok(key) = self.key(child)
            {
                return Some((at, index, key));
            }
        }
        None
    }

    fn binding_of(&self, at: usize) -> Option<crate::view::Binding> {
        self.arena(|view| Ok(view.nodes.get(at).and_then(|node| node.binding.clone())))
            .ok()
            .flatten()
    }

    /// Whether `at` edits the field `resource`'s type is named by.
    fn edits_title(&self, at: usize, resource: &str) -> bool {
        self.binding_of(at).is_some_and(|binding| {
            let BindTarget::Field { target, field } = &binding.target else {
                return false;
            };
            target.id_string() == resource
                && self.name_field(&target.ty).is_some_and(|id| &id == field)
        })
    }

    /// The field `ty` names its records by: its first text field.
    fn name_field(&self, ty: &str) -> Option<String> {
        let registry = self.registry.borrow();
        let (_, info) = registry.type_info(ty)?;
        Some(info.name_field()?.id.clone())
    }

    // ----- the arena --------------------------------------------------------------------

    fn props(&self, at: usize) -> Result<BTreeMap<String, Value>, RuntimeError> {
        self.arena(|view| {
            view.nodes
                .get(at)
                .map(|node| node.props.clone())
                .ok_or_else(|| "unknown ui node".to_owned())
        })
    }

    /// Set a prop of `at`.
    fn put_prop(&self, at: usize, name: &str, value: Value) -> Result<(), RuntimeError> {
        self.arena(|view| {
            let node = view
                .nodes
                .get_mut(at)
                .ok_or_else(|| "unknown ui node".to_owned())?;
            node.props.insert(name.to_owned(), value);
            Ok(())
        })
    }

    /// Take the prop `name` off `at`, when it carries one.
    fn remove_prop(&self, at: usize, name: &str) -> Result<(), RuntimeError> {
        self.arena(|view| {
            let node = view
                .nodes
                .get_mut(at)
                .ok_or_else(|| "unknown ui node".to_owned())?;
            node.props.remove(name);
            Ok(())
        })
    }

    /// Set what `at` does on `name`: the action the host runs when the control reports it.
    fn put_event(&self, at: usize, name: &str, action: Value) -> Result<(), RuntimeError> {
        self.arena(|view| {
            let node = view
                .nodes
                .get_mut(at)
                .ok_or_else(|| "unknown ui node".to_owned())?;
            node.events.insert(name.to_owned(), action);
            Ok(())
        })
    }

    fn kind(&self, at: usize) -> Result<String, RuntimeError> {
        self.arena(|view| {
            view.nodes
                .get(at)
                .map(|node| node.kind.clone())
                .ok_or_else(|| "unknown ui node".to_owned())
        })
    }

    fn children(&self, at: usize) -> Result<Vec<usize>, RuntimeError> {
        self.arena(|view| {
            view.nodes
                .get(at)
                .map(|node| node.children.clone())
                .ok_or_else(|| "unknown ui node".to_owned())
        })
    }

    fn key(&self, at: usize) -> Result<String, RuntimeError> {
        self.arena(|view| {
            view.nodes
                .get(at)
                .map(|node| node.key.clone())
                .ok_or_else(|| "unknown ui node".to_owned())
        })
    }

    /// `child` as a node of the tree, in the arena.
    fn push(&self, node: NodeBuild) -> Result<usize, RuntimeError> {
        self.arena(|view| {
            view.nodes.push(node);
            Ok(view.nodes.len().saturating_sub(1))
        })
    }

    /// Put `child` among `parent`'s children at `index` (the end for [`usize::MAX`]).
    fn insert(&self, parent: usize, index: usize, child: usize) -> Result<(), RuntimeError> {
        self.arena(|view| {
            let node = view
                .nodes
                .get_mut(parent)
                .ok_or_else(|| "unknown ui node".to_owned())?;
            let index = index.min(node.children.len());
            node.children.insert(index, child);
            Ok(())
        })
    }

    /// Take the child at `index` out of `parent`, leaving the node in the arena for another to
    /// hold.
    fn remove(&self, parent: usize, index: usize) -> Result<(), RuntimeError> {
        self.arena(|view| {
            let node = view
                .nodes
                .get_mut(parent)
                .ok_or_else(|| "unknown ui node".to_owned())?;
            if index < node.children.len() {
                node.children.remove(index);
            }
            Ok(())
        })
    }

    /// The slot `name` of `at`, when it has one.
    fn slot(&self, at: usize, name: &str) -> Result<Option<usize>, RuntimeError> {
        for child in self.children(at)? {
            let found = self.arena(|view| {
                let node = view
                    .nodes
                    .get(child)
                    .ok_or_else(|| "unknown ui node".to_owned())?;
                Ok((
                    node.kind.clone(),
                    node.props
                        .get("name")
                        .and_then(Value::as_text)
                        .map(str::to_owned),
                ))
            })?;
            if found.0 == SLOT && found.1.as_deref() == Some(name) {
                return Ok(Some(child));
            }
        }
        Ok(None)
    }

    /// `node` at the end of `at`'s slot `name`, the slot made when it has none.
    fn put_in_slot(&self, at: usize, name: &str, node: NodeBuild) -> Result<(), RuntimeError> {
        let child = self.push(node)?;
        self.put_at_in_slot(at, name, child)
    }

    /// The node `node` at the front of `at`'s slot `name`, the slot made when it has none: the
    /// window's main action leads the actions of its bar.
    fn put_in_slot_front(
        &self,
        at: usize,
        name: &str,
        node: NodeBuild,
    ) -> Result<(), RuntimeError> {
        let child = self.push(node)?;
        let key = self.key(at)?;
        let slot = if let Some(slot) = self.slot(at, name)? {
            slot
        } else {
            let mut props = BTreeMap::new();
            props.insert("name".to_owned(), Value::text(name));
            let mut slot = empty(SLOT, &format!("{key}~slot-{name}"));
            slot.props = props;
            let slot = self.push(slot)?;
            self.insert(at, usize::MAX, slot)?;
            slot
        };
        self.insert(slot, 0, child)
    }

    /// The node `child` (one of the arena's) at the end of `at`'s slot `name`, the slot made
    /// when it has none.
    fn put_at_in_slot(&self, at: usize, name: &str, child: usize) -> Result<(), RuntimeError> {
        let key = self.key(at)?;
        let slot = if let Some(slot) = self.slot(at, name)? {
            slot
        } else {
            let mut props = BTreeMap::new();
            props.insert("name".to_owned(), Value::text(name));
            let mut slot = empty(SLOT, &format!("{key}~slot-{name}"));
            slot.props = props;
            let slot = self.push(slot)?;
            self.insert(at, usize::MAX, slot)?;
            slot
        };
        self.insert(slot, usize::MAX, child)
    }

    /// One action as a menu row: what choosing it runs, in its words.
    fn menu_item(action: &Value, key: &str) -> NodeBuild {
        let mut node = empty("ui.MenuItem", key);
        node.props
            .insert("label".to_owned(), Value::text(Self::action_name(action)));
        if let Some(icon) = opt_text(action, ICON) {
            node.props.insert("icon".to_owned(), Value::text(icon));
        }
        if flag(action, DESTRUCTIVE) {
            node.props.insert("destructive".to_owned(), on_off(true));
        }
        if let Some(on) = opt_flag(action, ON) {
            node.props.insert("checked".to_owned(), on_off(on));
        }
        if let Some(shortcut) = opt_text(action, KEY) {
            node.props.insert("key".to_owned(), Value::text(shortcut));
        }
        if let Some(group) = opt_text(action, GROUP) {
            node.props.insert("group".to_owned(), Value::text(group));
        }
        if !flag(action, AVAILABLE) {
            node.props.insert("disabled".to_owned(), on_off(true));
            if let Some(reason) = opt_text(action, REASON) {
                node.props.insert("reason".to_owned(), Value::text(reason));
            }
        }
        if let Some(Value::Action(run)) = action_field(action, RUN) {
            node.events
                .insert("choose".to_owned(), Value::Action(run.clone()));
        }
        node
    }

    /// One action as a swipe: what a row offers when dragged, undone rather than confirmed.
    fn swipe_action(action: &Value, key: &str) -> NodeBuild {
        let mut node = empty("ui.SwipeAction", key);
        node.props
            .insert("label".to_owned(), Value::text(Self::action_name(action)));
        if let Some(icon) = opt_text(action, ICON) {
            node.props.insert("icon".to_owned(), Value::text(icon));
        }
        if flag(action, DESTRUCTIVE) {
            node.props.insert("destructive".to_owned(), on_off(true));
        }
        if let Some(Value::Action(run)) = action_field(action, RUN) {
            node.events
                .insert("swipe".to_owned(), Value::Action(run.clone()));
        }
        node
    }

    /// One action as a bar's control, as the standard library's header placed it: the primary
    /// labelled, a command that toggles a state a control of its own, all the rest behind the
    /// bar's door.
    /// `entry` is the key the list's entry declares, when the window has one: the primary it
    /// stands for is answered there, so it names the addition plainly rather than a form to
    /// fill in. A control of a selection's bar (`door` false) stands labelled among the others,
    /// whatever its place on one item's own bar was.
    fn action_button(action: &Value, key: &str, entry: Option<&str>, door: bool) -> NodeBuild {
        let primary = flag(action, PRIMARY);
        let destructive = flag(action, DESTRUCTIVE);
        let toggles = opt_flag(action, ON).is_some();
        let icon = opt_text(action, ICON).map(str::to_owned);
        let mut name = Self::action_name(action);
        if primary && entry.is_some() && entry == opt_text(action, KEY) {
            name = name.trim_end_matches('…').to_owned();
        }
        let mut node = empty("ui.Button", key);
        node.props.insert("label".to_owned(), Value::text(&name));
        if let Some(icon) = &icon {
            node.props.insert("icon".to_owned(), Value::text(icon));
        }
        // What a button takes of its action is the role the action declares: the window's
        // primary action is the primary button, and one that destroys what it names is drawn
        // as the destructive button it is wherever it stands, its own primary among them.
        if destructive {
            node.props
                .insert("role".to_owned(), Value::text("destructive"));
        } else if primary {
            node.props.insert("role".to_owned(), Value::text("primary"));
        }
        if door && !primary && toggles && icon.is_some() {
            node.props.insert("icon_only".to_owned(), on_off(true));
        }
        if door && !primary && !toggles {
            node.props.insert("in_menu".to_owned(), on_off(true));
            if let Some(group) = opt_text(action, GROUP) {
                node.props.insert("group".to_owned(), Value::text(group));
            }
        }
        if let Some(on) = opt_flag(action, ON) {
            node.props.insert("toggled".to_owned(), on_off(on));
        }
        if let Some(shortcut) = opt_text(action, KEY) {
            node.props.insert("key".to_owned(), Value::text(shortcut));
        }
        node.props
            .insert("enabled".to_owned(), on_off(flag(action, AVAILABLE)));
        if let Some(reason) = opt_text(action, REASON) {
            node.props
                .insert("description".to_owned(), Value::text(reason));
        }
        if let Some(Value::Action(run)) = action_field(action, RUN) {
            node.events
                .insert("press".to_owned(), Value::Action(run.clone()));
        }
        node
    }

    /// What an action is called, as the offer said: the words of its declaration, in the
    /// reader's language — those a view presenting its state showed where it has one — and the
    /// "…" of a command that still asks for something before it acts
    /// ([`crate::platform::Core::action_value`] settles both when it offers it).
    fn action_name(action: &Value) -> String {
        text_of(action_field(action, LABEL))
            .unwrap_or_default()
            .to_owned()
    }

    fn arena<T>(
        &self,
        f: impl FnOnce(&mut ViewState) -> Result<T, String>,
    ) -> Result<T, RuntimeError> {
        self.with_view(f).map_err(trapped)
    }
}

/// Why an import the host ran failed, as the view's own failure.
fn trapped(fail: crate::engine::HostFail) -> RuntimeError {
    match fail {
        crate::engine::HostFail::Trap(why) => RuntimeError::Package(why),
        crate::engine::HostFail::Suspend => {
            RuntimeError::Package("a view building its surfaces was suspended".to_owned())
        }
    }
}

/// A node of the tree, as the arena builds it.
fn empty(kind: &str, key: &str) -> NodeBuild {
    NodeBuild {
        kind: kind.to_owned(),
        key: key.to_owned(),
        props: BTreeMap::new(),
        children: Vec::new(),
        binding: None,
        events: BTreeMap::new(),
        anchor: None,
    }
}

/// A view's placement of one action: the action's id, the run of commands the view puts it in,
/// and whether the view makes it the primary one.
type PlacedAction = (String, Option<String>, bool);

/// A view's placement, in the order the view named the actions.
type Placed = [PlacedAction];

/// The actions a view placed on a node, when it did.
fn placement(props: &BTreeMap<String, Value>) -> Option<Vec<PlacedAction>> {
    let Value::List(entries) = props.get(PLACEMENT)? else {
        return None;
    };
    let mut placed = Vec::with_capacity(entries.len());
    for entry in entries {
        let Value::Tuple(fields) = entry else {
            continue;
        };
        let Some(id) = fields.get(PLACED_ID).and_then(Value::as_text) else {
            continue;
        };
        let group = match fields.get(PLACED_GROUP) {
            Some(Value::Opt(Some(held))) => held.as_text().map(str::to_owned),
            held => held.and_then(Value::as_text).map(str::to_owned),
        };
        let primary = match fields.get(PLACED_PRIMARY) {
            Some(Value::Bool(flag)) => *flag,
            Some(Value::Int(n)) => *n != 0,
            _ => false,
        };
        placed.push((id.to_owned(), group, primary));
    }
    Some(placed)
}

fn action_field(action: &Value, at: usize) -> Option<&Value> {
    match action {
        Value::Tuple(fields) => fields.get(at),
        _ => None,
    }
}

fn text_of(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::Text(text)) => Some(text.as_str()),
        _ => None,
    }
}

/// A boolean prop as a tree holds it: a `Bool` prop a view writes is `1` or `0`.
fn on_off(yes: bool) -> Value {
    Value::Int(i64::from(yes))
}

/// A boolean prop as a node holds it, off when it is not there.
fn flag_prop(props: &BTreeMap<String, Value>, name: &str) -> bool {
    match props.get(name) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Int(n)) => *n != 0,
        Some(Value::Opt(Some(held))) => matches!(**held, Value::Bool(true)),
        _ => false,
    }
}

/// The resource a node's prop `name` names, and none for anything else.
fn ref_prop(props: &BTreeMap<String, Value>, name: &str) -> Option<RefValue> {
    match props.get(name) {
        Some(Value::Ref(item)) => Some(item.clone()),
        Some(Value::Opt(Some(held))) => match &**held {
            Value::Ref(item) => Some(item.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn flag(action: &Value, at: usize) -> bool {
    match action_field(action, at) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Int(n)) => *n != 0,
        Some(Value::Opt(Some(held))) => matches!(**held, Value::Bool(true)),
        _ => false,
    }
}

fn opt_flag(action: &Value, at: usize) -> Option<bool> {
    match action_field(action, at) {
        Some(Value::Opt(Some(held))) => match &**held {
            Value::Bool(flag) => Some(*flag),
            Value::Int(n) => Some(*n != 0),
            _ => None,
        },
        Some(Value::Bool(flag)) => Some(*flag),
        _ => None,
    }
}

fn opt_text(action: &Value, at: usize) -> Option<&str> {
    match action_field(action, at) {
        Some(Value::Opt(Some(held))) => text_of(Some(held)),
        value => text_of(value),
    }
    .filter(|text| !text.is_empty())
}
