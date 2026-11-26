//! [`StatusLine`] and [`ObjectHeader`] as described components.

use crate::widget::list_row::RowStatus;
use tilo_ui_base::icons::Icon;
use tilo_ui_base::{component::Component, component::Described, component::Person};

use super::{ObjectHeader, ObjectLead, StatusLine};

tilo_ui_base::choice!(RowStatus, "RowStatus", {
    Active => "active",
    Warning => "warning",
    Error => "error",
    Idle => "idle",
});

/// A status as a sentence: a dot in the status's colour beside a caption ("Engine running", "3
/// containers stopped"), for a list's footer or a detail's state; it is never pressed.
#[derive(Component, Default)]
#[component(name = "StatusLine", role = "StaticText", label = "text")]
pub struct StatusLineProps {
    /// The sentence: "Synced 2 minutes ago".
    #[component(required, positional)]
    pub text: String,
    /// What state it says: `active` (running, connected, healthy) unless said, `warning` (needs
    /// attention soon), `error` (failed or offline unexpectedly), `idle` (present but
    /// inactive).
    pub status: Option<RowStatus>,
    /// A person whose presence it says, in their colour, instead of a status.
    pub who: Option<Person>,
}

fn said(props: StatusLineProps) -> (RowStatus, String) {
    let StatusLineProps { text, status, who } = props;
    let status = match (who, status) {
        (Some(who), _) => RowStatus::Presence(who.hue),
        (None, Some(status)) => status,
        (None, None) => RowStatus::Active,
    };
    (status, text)
}

impl Described for StatusLine {
    type Props = StatusLineProps;

    fn from_props(_key: &str, props: StatusLineProps) -> Self {
        let (status, text) = said(props);
        Self::with(status, text)
    }

    /// The sentence and the state it says change where the line stands: a live count that
    /// ticks, a connection that fails.
    fn set_props(&mut self, props: StatusLineProps) -> bool {
        let (status, text) = said(props);
        self.set_status(status, text);
        true
    }
}

/// The head of a detail view: an avatar or glyph, the thing's name and one caption line under
/// it ("Ada Lovelace" over "ada@example.org").
#[derive(Component, Default)]
#[component(name = "ObjectHeader", role = "Group", label = "title")]
pub struct ObjectHeaderProps {
    /// The thing's name.
    #[component(required, positional)]
    pub title: String,
    /// One line under the name: an address, a URL, a kind.
    pub caption: Option<String>,
    /// A person before the name, as their avatar.
    pub who: Option<Person>,
    /// A glyph before the name: the kind of thing it is.
    pub icon: Option<Icon>,
    /// The avatar is square: an account or a site, not a person.
    pub square: bool,
}

impl Described for ObjectHeader {
    type Props = ObjectHeaderProps;

    fn set_props(&mut self, props: ObjectHeaderProps) -> bool {
        let ObjectHeaderProps {
            title,
            caption,
            who,
            icon,
            square,
        } = props;
        let lead = match (who, icon) {
            (Some(who), _) => Some(ObjectLead::Avatar {
                name: who.name,
                identity: who.identity.id().to_owned(),
                image: None,
                square,
            }),
            (None, Some(icon)) => Some(ObjectLead::Icon(icon)),
            (None, None) => None,
        };
        self.set_object(title, caption, lead);
        true
    }

    fn from_props(_key: &str, props: ObjectHeaderProps) -> Self {
        let ObjectHeaderProps {
            title,
            caption,
            who,
            icon,
            square,
        } = props;
        let mut header = Self::new(title);
        if let Some(caption) = caption {
            header = header.caption(caption);
        }
        match (who, icon) {
            (Some(who), _) => {
                header = header.avatar(who.name, who.identity.id());
                if square {
                    header = header.square();
                }
            }
            (None, Some(icon)) => header = header.icon(icon),
            (None, None) => {}
        }
        header
    }
}
