//! [`NavShell`] as a described component.

use crate::component::{Component, Described, Event, PropType, Type, Value};
use crate::widget::Element;

use super::{ContentKind, NavPane, NavShell};

/// The one window shell: a sidebar of places, an overview list, the content and an inspector,
/// side by side while they fit and one at a time, with Back, when they do not. The header of
/// each column holds its title, its count and its actions.
#[derive(Component, Default)]
#[component(name = "NavShell", role = "Window", label = "title")]
pub struct NavShellProps {
    /// What the content is, which sets the narrowest it still works at: `page` unless said.
    pub content_kind: ContentKind,
    /// The content's title, in its header.
    pub title: Option<String>,
    /// How many things the content lists, beside its title.
    pub count: Option<i64>,
    /// The overview column's title.
    pub overview_title: Option<String>,
    /// How many things the overview lists, beside its title.
    pub overview_count: Option<i64>,
    /// Whether the content's Back goes back now (a drill-down the app keeps).
    pub can_go_back: bool,
    /// The places of the app: a navigation list.
    #[component(slot)]
    pub sidebar: Option<Element>,
    /// The list the content is chosen from.
    #[component(slot)]
    pub overview: Option<Element>,
    /// What the window is for.
    #[component(slot(default))]
    pub content: Option<Element>,
    /// Details of what the content shows, beside it.
    #[component(slot)]
    pub inspector: Option<Element>,
    /// The content's actions, in its header.
    #[component(slot)]
    pub actions: Vec<Element>,
    /// The column in front of a narrow window.
    #[component(state)]
    pub pane: Option<NavPane>,
    /// The content's Back was pressed.
    #[component(event)]
    pub back: Event,
}

impl PropType for ContentKind {
    fn ty() -> Type {
        Type::Choice {
            name: "ContentKind".into(),
            cases: ["list", "reading", "page", "grid", "document", "canvas"]
                .map(String::from)
                .to_vec(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let (Value::Choice(case) | Value::Text(case)) = value else {
            return Err(format!("takes ContentKind, not {}", value.kind()));
        };
        Ok(match case.as_str() {
            "list" => Self::List,
            "reading" => Self::Reading,
            "page" => Self::Page,
            "grid" => Self::Grid,
            "document" => Self::Document,
            "canvas" => Self::Canvas,
            other => return Err(format!("has no case `{other}`")),
        })
    }
}

impl PropType for NavPane {
    fn ty() -> Type {
        Type::Choice {
            name: "NavPane".into(),
            cases: ["sidebar", "overview", "content"]
                .map(String::from)
                .to_vec(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let (Value::Choice(case) | Value::Text(case)) = value else {
            return Err(format!("takes NavPane, not {}", value.kind()));
        };
        Ok(match case.as_str() {
            "sidebar" => Self::Sidebar,
            "overview" => Self::Overview,
            "content" => Self::Content,
            other => return Err(format!("has no case `{other}`")),
        })
    }
}

fn count_of(count: Option<i64>) -> Option<usize> {
    count.map(|count| usize::try_from(count.max(0)).unwrap_or(usize::MAX))
}

impl Described for NavShell {
    type Props = NavShellProps;

    fn from_props(key: &str, props: NavShellProps) -> Self {
        let NavShellProps {
            content_kind,
            title,
            count,
            overview_title,
            overview_count,
            can_go_back,
            sidebar,
            overview,
            content,
            inspector,
            actions,
            pane,
            back,
        } = props;
        let mut shell = Self::new().with_id(key).content_kind(content_kind);
        if let Some(sidebar) = sidebar {
            shell = shell.sidebar(sidebar);
        }
        if let Some(overview) = overview {
            shell = shell.overview(overview);
        }
        if let Some(content) = content {
            shell = shell.content(content);
        }
        if let Some(inspector) = inspector {
            shell = shell.inspector(inspector);
        }
        if title.is_some() || !actions.is_empty() {
            shell = shell.content_header(title.unwrap_or_default(), actions);
        }
        if let Some(count) = count_of(count) {
            shell = shell.content_count(count);
        }
        if let Some(title) = overview_title {
            shell = shell.overview_header(title, Vec::new());
        }
        if let Some(count) = count_of(overview_count) {
            shell = shell.overview_count(count);
        }
        if let Some(mut go_back) = back.handler() {
            shell = shell.on_back(move || can_go_back, move || go_back());
        }
        if let Some(pane) = pane {
            shell.show(pane);
            shell.snap();
        }
        shell
    }
}
