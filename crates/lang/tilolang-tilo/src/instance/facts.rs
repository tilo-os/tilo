//! The facts page: the page the platform shows for a resource whose type declares no default view.
//!
//! A press on a row that presents a resource is one thing: show the resource in its type's
//! default view ([`Instance::open_detail`](super::Instance)). An app may list a resource of a
//! type it gives no view of (a tag, a row of a table, a card's item), and an agent or a search
//! hit may name one; the platform still shows it, as the app's own page would: what the
//! resource is called, the type it is, and its fields as facts in the person's own formats. It
//! stands at the window's detail level, so it has the Back step every page there has, and no
//! press ever says the app could not be opened.

use std::collections::BTreeMap;

use tilo_render_protocol::patch::{self as ui};

use crate::runtime::{ResourceRef, Runtime, Value};

/// The key prefix of a facts page's nodes in the window.
pub const KEY: &str = "facts~";

/// A resource the platform shows in its facts page: the detail level when the resource's type
/// declares no view to draw it with ([`page`]).
pub(super) struct Facts {
    pub(super) target: ResourceRef,
    /// The tree the window holds, as last sent: patches are against it.
    pub(super) sent: ui::Node,
}

impl super::Instance {
    /// The resource the detail level shows: its view's, or the facts page's.
    #[must_use]
    pub fn detail_target(&self) -> Option<&ResourceRef> {
        self.detail
            .as_ref()
            .map(|view| &view.target)
            .or_else(|| self.facts.as_ref().map(|facts| &facts.target))
    }

    /// The facts page the detail level shows, as the window holds it, when the level shows one.
    #[must_use]
    pub fn detail_facts(&self) -> Option<&ui::Node> {
        self.facts.as_ref().map(|facts| &facts.sent)
    }

    /// A new build of the app: its facts page opens again through what the build declares, or as
    /// the page again when it still declares no view of the type.
    pub(super) fn reopen_facts(&mut self) {
        let Some(target) = self.facts.as_ref().map(|facts| facts.target.clone()) else {
            return;
        };
        self.facts = None;
        self.open_detail(&target, None, None, 0);
    }
}

/// The page of `target`, drawn from the type's own schema: `details` is what its section of
/// facts is called in the person's language. `None` for a resource whose type the runtime holds
/// no package of (a reference to something gone), which no page can say anything of.
pub(super) fn page(runtime: &Runtime, target: &ResourceRef, details: &str) -> Option<ui::Node> {
    let kind = runtime.type_name(&target.ty)?;
    let title = runtime.title(target).unwrap_or_default();
    let title = if title.trim().is_empty() {
        kind.clone()
    } else {
        title
    };
    let mut facts = Vec::new();
    for (field, ty) in runtime.fields_of(&target.ty) {
        let Ok(value) = runtime.read(target, &field) else {
            continue;
        };
        // A collection says how many it holds: what a page of facts can say of many things.
        let shown = match &value {
            Value::List(items) => {
                let count = items.len();
                if count == 0 {
                    continue;
                }
                format!("{count}")
            }
            value => runtime.shown_value(value, &ty),
        };
        let shown = one_line(&shown);
        if shown.is_empty() || shown == title {
            continue;
        }
        facts.push(property(&field, &shown));
    }
    Some(tree(&title, &kind, details, facts))
}

/// The page: the resource's name as the window's title, what it is, and its facts.
fn tree(title: &str, kind: &str, details: &str, facts: Vec<ui::Node>) -> ui::Node {
    let mut about = node("stack", &format!("{KEY}about"));
    about
        .props
        .insert("gap".to_owned(), ui::Value::Enum("tight".to_owned()));
    about.children.push(title_node(title));
    about.children.push(caption(kind));
    let mut body = node("stack", &format!("{KEY}body"));
    body.props
        .insert("gap".to_owned(), ui::Value::Enum("group".to_owned()));
    body.children.push(page_of(about));
    if !facts.is_empty() {
        body.children.push(section(details, facts));
    }
    let mut scroll = node("scroll", &format!("{KEY}scroll"));
    scroll.children.push(body);
    let mut shell = node("ui.NavShell", &format!("{KEY}shell"));
    shell.props.insert(
        "content_kind".to_owned(),
        ui::Value::Enum("document".to_owned()),
    );
    shell
        .props
        .insert("title".to_owned(), ui::Value::some(ui::Value::text(title)));
    let mut content = node("part", &format!("{KEY}content"));
    content
        .props
        .insert("name".to_owned(), ui::Value::text("content"));
    content.children.push(scroll);
    shell.children.push(content);
    shell
}

/// The facts of the resource, under the section `title`.
fn section(title: &str, facts: Vec<ui::Node>) -> ui::Node {
    let mut section = node("ui.Section", &format!("{KEY}facts"));
    section
        .props
        .insert("title".to_owned(), ui::Value::some(ui::Value::text(title)));
    // The facts are a property list's rows: a label beside each value, the platform's own way of
    // saying what a thing holds. Its one part is its default, so they stand as its children.
    let mut list = node("ui.PropertyList", &format!("{KEY}properties"));
    list.children = facts;
    section.children.push(list);
    section
}

/// The resource's name as the page's title.
fn title_node(title: &str) -> ui::Node {
    let mut node = node("ui.Text", &format!("{KEY}title"));
    node.props
        .insert("value".to_owned(), ui::Value::text(title));
    node.props
        .insert("role".to_owned(), ui::Value::Enum("title".to_owned()));
    node
}

/// What the resource is: its type, in secondary ink under the title.
fn caption(kind: &str) -> ui::Node {
    let mut node = node("ui.Text", &format!("{KEY}kind"));
    node.props.insert("value".to_owned(), ui::Value::text(kind));
    node.props
        .insert("role".to_owned(), ui::Value::Enum("caption".to_owned()));
    node.props
        .insert("secondary".to_owned(), ui::Value::Bool(true));
    node
}

/// What a fact says, in one line: its first line, cut at the length a card shows of a document's
/// text. A body of writing is a fact by its opening, never a wall in a list of facts.
fn one_line(shown: &str) -> String {
    let line = shown
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim();
    let mut out: String = line.chars().take(super::PREVIEW_CHARS).collect();
    if line.chars().count() > super::PREVIEW_CHARS {
        out.push('…');
    }
    out
}

/// One fact of the resource: its field's name and the value it holds.
fn property(field: &str, value: &str) -> ui::Node {
    let mut node = node("ui.Property", &format!("{KEY}{field}"));
    node.props.insert(
        "label".to_owned(),
        ui::Value::text(crate::runtime::platform::sentence(field)),
    );
    node.props
        .insert("value".to_owned(), ui::Value::text(value));
    node
}

/// `content` at the readable measure: what `std.page` gives a page's body.
fn page_of(content: ui::Node) -> ui::Node {
    let mut page = node("page", &format!("{KEY}measure"));
    let mut stack = node("stack", &format!("{KEY}measure~stack"));
    stack
        .props
        .insert("gap".to_owned(), ui::Value::Enum("group".to_owned()));
    stack.children.push(content);
    page.children.push(stack);
    page
}

fn node(kind: &str, key: &str) -> ui::Node {
    ui::Node {
        kind: kind.to_owned(),
        key: key.to_owned(),
        props: BTreeMap::new(),
        binding: None,
        events: BTreeMap::new(),
        anchor: None,
        children: Vec::new(),
    }
}
