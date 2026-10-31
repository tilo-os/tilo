//! [`DropZone`] as a described component.

use std::path::PathBuf;

use crate::component::{Component, Described, Event, Value};
use crate::icons::Icon;

use super::{Button, DropZone};

/// Where files go: drag them here, or press the button to choose them (an import, a picture to
/// swap, a file to open). At rest it is a dashed edge around a glyph and a title; while a drag
/// hovers it takes your colour.
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
    /// Files were dropped: carries their paths, as `List<Text>`.
    #[component(event(value = Vec<String>))]
    pub drop: Event,
    /// The action's button was pressed: the view asks for the files its own way.
    #[component(event)]
    pub choose: Event,
}

impl Described for DropZone {
    type Props = DropZoneProps;

    fn from_props(key: &str, props: DropZoneProps) -> Self {
        let DropZoneProps {
            title,
            message,
            icon,
            action,
            drop,
            choose,
        } = props;
        let mut zone = Self::new(key.to_owned(), title);
        if let Some(message) = message {
            zone = zone.message(message);
        }
        if let Some(icon) = icon {
            zone = zone.icon(icon);
        }
        if let Some(label) = action {
            let button = Button::new(&format!("{key}.action"), &label);
            zone = zone.with_action(match choose.clone() {
                Event::None => button,
                Event::Action(binding) => button.on_action(binding),
                Event::View(intent) => button.on_view(intent.sender()),
            });
        }
        if !drop.is_none() {
            zone = zone.on_files(move |files: Vec<PathBuf>| {
                let paths: Vec<Value> = files
                    .into_iter()
                    .map(|path| Value::Text(path.display().to_string()))
                    .collect();
                drop.fire(Some(Value::List(paths)));
            });
        }
        zone
    }
}
