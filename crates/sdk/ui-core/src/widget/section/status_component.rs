//! [`StatusLine`] and [`ObjectHeader`] as described components.

use crate::component::{Component, Described, Person};
use crate::icons::Icon;
use crate::widget::list_row::RowStatus;

use super::{ObjectHeader, OwnedStatus, StatusLine};

crate::component::choice!(RowStatus<'static>, "RowStatus", {
    Active => "active",
    Warning => "warning",
    Error => "error",
    Idle => "idle",
});

/// A status in a sentence: a dot in the status's colour and a caption beside it, "Engine
/// running", "3 containers stopped". For a list's footer or a detail's state; it is never
/// pressed.
#[derive(Component, Default)]
#[component(name = "StatusLine", role = "StaticText", label = "text")]
pub struct StatusLineProps {
    /// The sentence: "Synced 2 minutes ago".
    #[component(required, positional)]
    pub text: String,
    /// What state it says: `active` (running, connected, healthy) unless said, `warning`
    /// (needs attention soon), `error` (failed, or offline unexpectedly), `idle` (present but
    /// inactive).
    pub status: Option<RowStatus<'static>>,
    /// A person whose presence it says, in their colour, instead of a status.
    pub who: Option<Person>,
}

impl Described for StatusLine {
    type Props = StatusLineProps;

    fn from_props(_key: &str, props: StatusLineProps) -> Self {
        let StatusLineProps { text, status, who } = props;
        let status = match (who, status) {
            (Some(who), _) => OwnedStatus::Presence(who.identity),
            (None, Some(RowStatus::Warning)) => OwnedStatus::Warning,
            (None, Some(RowStatus::Error)) => OwnedStatus::Error,
            (None, Some(RowStatus::Idle)) => OwnedStatus::Idle,
            (None, Some(RowStatus::Active | RowStatus::Presence(_)) | None) => OwnedStatus::Active,
        };
        Self::with(status, text)
    }
}

/// The head of a detail view: an avatar or a glyph, the thing's name as a title and one
/// caption line under it, "Ada Lovelace" over "ada@example.org", a password's site over its
/// address.
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
                header = header.avatar(who.name, who.identity);
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
