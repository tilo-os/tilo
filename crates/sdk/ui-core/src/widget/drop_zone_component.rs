//! [`DropZone`] as a described component.

use std::path::PathBuf;

use crate::component::{Component, Described, Event, Value};
use crate::icons::Icon;

use super::{Button, DropZone};

/// Where files go: dragged here, or chosen with the button.
#[derive(Component, Default)]
#[component(name = "DropZone", role = "Group", label = "title")]
pub struct DropZoneProps {
    /// What the zone takes, in a sentence: "Drop files to import".
    #[component(required, positional)]
    pub title: String,
    /// A line under the title saying what it accepts: "PNG or JPEG up to 50 MB".
    pub message: Option<String>,
    /// A glyph for what it takes, a download arrow unless said.
    pub icon: Option<Icon>,
    /// The label of a button offering the same route without dragging ("Choose files").
    pub action: Option<String>,
    /// Files were dropped: their paths, as `List<Text>`.
    #[component(event(value = Vec<String>))]
    pub drop: Event,
    /// The action's button was pressed: the view asks for the files its own way.
    #[component(event(press))]
    pub choose: Event,
}

impl Described for DropZone {
    type Props = DropZoneProps;

    fn from_props(key: &str, props: DropZoneProps) -> Self {
        let mut zone = Self::new(key.to_owned(), "");
        zone.set_content(props);
        zone
    }

    fn set_props(&mut self, props: DropZoneProps) -> bool {
        self.set_content(props);
        true
    }
}

impl DropZone {
    fn set_content(&mut self, props: DropZoneProps) {
        let DropZoneProps {
            title,
            message,
            icon,
            action,
            drop,
            choose,
        } = props;
        self.title = title;
        self.message = message;
        self.icon = icon.unwrap_or(Icon::Download);
        self.action = action.map(|label| {
            let button = Button::new(&format!("{}.action", self.id), &label);
            match choose.clone() {
                Event::None => button,
                Event::Action(binding) => button.on_action(binding),
                Event::View(intent) => button.on_view(intent.sender()),
            }
        });
        if drop.is_none() {
            self.on_files = None;
        } else {
            self.set_on_files_view(move |files: Vec<PathBuf>| {
                let paths: Vec<Value> = files
                    .into_iter()
                    .map(|path| Value::Text(path.display().to_string()))
                    .collect();
                drop.fire(Some(Value::List(paths)));
            });
        }
        self.action_node = None;
        self.metrics = None;
        self.dirty = true;
    }
}
