//! [`DropZone`] as a described component.

use std::path::PathBuf;

use tilo_ui_base::icons::Icon;
use tilo_ui_base::{
    component::Component, component::Described, component::Event, component::Value,
};

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
    /// Files were dropped: each as a `File` — its name, its media type, its size and where its
    /// bytes are — kept by the window, which is what an app reads of it.
    #[component(event(value = Vec<tilo_ui_base::component::File>))]
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
                Event::View(intent) => button.on_open(intent.sender()),
            }
        });
        if drop.is_none() {
            self.on_files = None;
        } else {
            self.set_on_files(move |files: Vec<PathBuf>| {
                // A dropped file goes into the content store, and the view hears of it as the
                // `File` an app's process can open; one the store will not keep says why where
                // the person dropped it.
                let mut kept = Vec::new();
                for path in files {
                    match crate::files::dropped_file(&path) {
                        Ok(file) => kept.push(Value::File(file)),
                        Err(refusal) => crate::control_notice::say_in_window(&refusal.words()),
                    }
                }
                if !kept.is_empty() {
                    drop.fire(Some(Value::List(kept)));
                }
            });
        }
        self.action_node = None;
        self.metrics = None;
        self.dirty = true;
    }
}
