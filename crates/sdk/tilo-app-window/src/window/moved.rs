//! The app's commands the person moved to other keys (`settings.shortcuts` `rebind-command`), as
//! the window answers a press: a moved command runs on its new keys and no longer on the ones its
//! app gave it, whichever control or published action declares it. Every app's window reads the
//! one table the person keeps, so a command moved once is moved in every window of its app.

use std::cell::RefCell;
use ui::time::{Duration, Instant};

/// How long a reading of the person's table is answered before it is read again: a move in
/// Settings reaches a window that is open within this.
const FRESH: Duration = Duration::from_millis(500);

/// One command the person moved: the action's id, and the keys it is on now (`None`: on none).
pub(crate) type Moved = (String, Option<String>);

/// A moved command's keys as its app declared them, and the keys it is on now.
pub(crate) type Relabels = Vec<(String, Option<String>)>;

/// `node` as the person sees it with `relabels` applied: a control that shows keys a moved command
/// left shows the keys it is on now, or none. Matching a press reads the tree as the app declared
/// it; only what is drawn shows the person's keys.
pub(crate) fn relabeled<'a>(
    node: &'a crate::UiNode,
    relabels: &[(String, Option<String>)],
) -> std::borrow::Cow<'a, crate::UiNode> {
    if relabels.is_empty() {
        return std::borrow::Cow::Borrowed(node);
    }
    let mut node = node.clone();
    relabel(&mut node, relabels);
    std::borrow::Cow::Owned(node)
}

fn relabel(node: &mut crate::UiNode, relabels: &[(String, Option<String>)]) {
    use crate::Value;
    let now = |keys: &str| -> Option<Option<String>> {
        let pressed = tilo_ui_base::platform::Shortcut::from(keys);
        relabels
            .iter()
            .find(|(declared, _)| {
                tilo_ui_base::platform::Shortcut::from(declared.as_str()).same_press(&pressed)
            })
            .map(|(_, now)| now.clone())
    };
    if let Some(Value::Text(keys)) = node.props.get("key")
        && let Some(moved) = now(keys)
    {
        match moved {
            Some(keys) => {
                node.props.insert("key".to_owned(), Value::Text(keys));
            }
            None => {
                node.props.remove("key");
            }
        }
    }
    if let Some(Value::List(keys)) = node.props.get_mut("keys") {
        keys.retain_mut(|key| match key {
            Value::Text(text) => match now(text) {
                Some(Some(moved)) => {
                    *text = moved;
                    true
                }
                Some(None) => false,
                None => true,
            },
            _ => true,
        });
    }
    for child in &mut node.children {
        relabel(child, relabels);
    }
}

/// Where the window reads the moved commands from.
#[derive(Default)]
pub(crate) struct MovedCommands {
    /// What a host or a test gave the window, read instead of the person's table.
    given: Option<Vec<Moved>>,
    /// The person's table as last read for the window's app, and when.
    read: RefCell<Option<(Instant, String, Vec<Moved>)>>,
}

/// What a press is, once the person's moves are read.
pub(crate) enum MovedPress {
    /// No moved command is on it or was on it: the press as it came.
    Unmoved,
    /// The new keys of a moved command: the press the app declared the command on.
    To(tilo_ui_base::platform::Shortcut),
    /// Keys the app gave a command the person moved off them: no press of the app's.
    Off,
}

impl MovedCommands {
    /// Answer `moved` instead of the person's table.
    pub(crate) fn give(&mut self, moved: Vec<Moved>) {
        self.given = Some(moved);
    }

    /// The commands of `app` the person moved.
    fn of(&self, app: &str) -> Vec<Moved> {
        if let Some(given) = &self.given {
            return given.clone();
        }
        if app.is_empty() {
            return Vec::new();
        }
        let now = ui::time::now();
        let mut read = self.read.borrow_mut();
        if let Some((at, read_for, moved)) = read.as_ref()
            && read_for == app
            && now.saturating_duration_since(*at) < FRESH
        {
            return moved.clone();
        }
        let config = ui::shortcut_table::ShortcutConfig::load();
        let moved: Vec<Moved> = config
            .commands_of(app)
            .map(|bound| {
                (
                    bound.action.clone(),
                    bound
                        .chord
                        .as_ref()
                        .map(ui::shortcut_table::KeyChord::display),
                )
            })
            .collect();
        *read = Some((now, app.to_owned(), moved.clone()));
        moved
    }

    /// What `pressed` is in `app`'s window, whose actions declare their keys as `keys_of` reads
    /// them.
    pub(crate) fn press(
        &self,
        app: &str,
        pressed: &tilo_ui_base::platform::Shortcut,
        keys_of: impl Fn(&str) -> Option<String>,
    ) -> MovedPress {
        let moved = self.of(app);
        if moved.is_empty() {
            return MovedPress::Unmoved;
        }
        let on = |chord: &str| tilo_ui_base::platform::Shortcut::from(chord).same_press(pressed);
        // The new keys first: a command moved onto keys another moved command left is run there.
        if let Some(original) = moved
            .iter()
            .filter(|(_, chord)| chord.as_deref().is_some_and(on))
            .find_map(|(action, _)| keys_of(action))
        {
            return MovedPress::To(tilo_ui_base::platform::Shortcut::from(original.as_str()));
        }
        if moved
            .iter()
            .any(|(action, _)| keys_of(action).is_some_and(|keys| on(&keys)))
        {
            return MovedPress::Off;
        }
        MovedPress::Unmoved
    }

    /// How the window's controls show the moved commands' keys: each moved command's keys as its
    /// app declared them (`keys_of`), and the keys it is on now.
    pub(crate) fn relabels(&self, app: &str, keys_of: impl Fn(&str) -> Option<String>) -> Relabels {
        self.of(app)
            .into_iter()
            .filter_map(|(action, now)| keys_of(&action).map(|declared| (declared, now)))
            .collect()
    }

    /// The commands of `app` the person moved, as the window's palette shows them.
    pub(crate) fn now(&self, app: &str) -> Vec<Moved> {
        self.of(app)
    }

    /// The keys the command `action`, declared on `declared`, is on now.
    pub(crate) fn keys(&self, app: &str, action: &str, declared: Option<&str>) -> Option<String> {
        match self.of(app).into_iter().find(|(moved, _)| moved == action) {
            Some((_, chord)) => chord,
            None => declared.map(str::to_owned),
        }
    }
}
