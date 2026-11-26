//! A composition as a web page: semantic HTML that the browser lays out itself.
//!
//! The markup carries no geometry of the frame it was composed in. A container is
//! a CSS flex or grid box with its layout node's style, and its children flow
//! inside it. Text is text, a field is an `<input>` or `<textarea>`, a document's
//! blocks are paragraphs, headings and list items, and a control is the
//! browser's own element. So the page lays out at any width, and the server
//! that renders it never needs to know the window's size. The one exception
//! is a widget that is its own paint: an island the page fills with its
//! picture.
//!
//! Every element the page acts on carries `data-p`, a short number that
//! [`Page`] keeps for a widget's path from one render to the next. What the
//! person does on it comes back as the widget's semantic action: `data-a` is
//! the action (`activate`, `toggle`, `value`, `step`, `edit`), `data-t` the
//! semantic target and `data-v` the value it sets. [`Page::render`] answers
//! the whole document body. [`Page::patch`] answers only the elements that
//! changed since the last render, each replacing the element with its
//! `data-p`.

use std::collections::HashMap;
use std::fmt::Write as _;

use super::{
    CheckStyle, Composed, Composition, Control, ControlOption, DocBlock, Face, Glyph, InputKind,
    Own, TextLook,
};
use crate::a11y::{AccessNode, Role};

/// One page's render of a window: the numbers its elements carry, kept by
/// path, and the elements it last sent, so the next render answers only what
/// changed.
#[derive(Default)]
pub struct Page {
    ids: HashMap<String, u32>,
    paths: Vec<String>,
    last: Option<Element>,
    /// Widgets whose children their html places at rects instead of flowing
    /// them. The page flows them anyway; this is the HTML they still lack.
    pub placed: Vec<String>,
    /// What the page's host shows in each well, by place ([`Composed::well`]): the markup
    /// that stands where the compositor would show the place's window natively (a link to the
    /// app's own page). A well the host fills with nothing stays empty.
    pub wells: HashMap<String, String>,
}

/// One element replaced by its new HTML: what [`Page::patch`] answers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Patch {
    pub id: u32,
    pub html: String,
}

/// One element of the rendered page: its opening tag, what it shows of its own,
/// and its keyed children.
#[derive(Clone, Debug, PartialEq)]
struct Element {
    id: Option<u32>,
    tag: &'static str,
    attrs: String,
    /// Markup before its children (a face's words, a field's value).
    own: String,
    children: Vec<Element>,
}

impl Element {
    fn new(tag: &'static str) -> Self {
        Self {
            id: None,
            tag,
            attrs: String::new(),
            own: String::new(),
            children: Vec::new(),
        }
    }

    fn attr(&mut self, name: &str, value: &str) {
        let _ = write!(self.attrs, " {name}=\"{}\"", escape(value));
    }

    fn flag(&mut self, name: &str) {
        let _ = write!(self.attrs, " {name}");
    }

    fn write(&self, out: &mut String) {
        out.push('<');
        out.push_str(self.tag);
        if let Some(id) = self.id {
            let _ = write!(out, " data-p=\"{id}\"");
        }
        out.push_str(&self.attrs);
        out.push('>');
        if VOID.contains(&self.tag) {
            return;
        }
        out.push_str(&self.own);
        for child in &self.children {
            child.write(out);
        }
        let _ = write!(out, "</{}>", self.tag);
    }

    fn html(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    /// The elements of `self` that differ from `old`, outermost first: an element
    /// whose own markup or whose children changed is replaced whole. One with no
    /// number of its own is replaced as part of the nearest one that has.
    fn diff(&self, old: &Self, out: &mut Vec<Patch>) -> bool {
        let same_shape = self.tag == old.tag
            && self.id == old.id
            && self.attrs == old.attrs
            && self.own == old.own
            && self.children.len() == old.children.len()
            && self
                .children
                .iter()
                .zip(&old.children)
                .all(|(new, old)| new.id == old.id && new.tag == old.tag);
        if !same_shape {
            return false;
        }
        for (new, old) in self.children.iter().zip(&old.children) {
            if new == old {
                continue;
            }
            let mut inner = Vec::new();
            if new.diff(old, &mut inner) {
                out.extend(inner);
            } else if let Some(id) = new.id {
                out.push(Patch {
                    id,
                    html: new.html(),
                });
            } else {
                return false;
            }
        }
        true
    }
}

const VOID: [&str; 5] = ["input", "br", "hr", "img", "wbr"];

impl Page {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The number a widget's element carries, for its `path`.
    fn id(&mut self, path: &str) -> u32 {
        if let Some(id) = self.ids.get(path) {
            return *id;
        }
        let id = u32::try_from(self.paths.len()).unwrap_or(u32::MAX);
        self.paths.push(path.to_owned());
        self.ids.insert(path.to_owned(), id);
        id
    }

    /// The path of the widget whose element carries `id`.
    #[must_use]
    pub fn path(&self, id: u32) -> Option<&str> {
        self.paths.get(id as usize).map(String::as_str)
    }

    /// The whole page body for `composition`, which [`Self::patch`] later
    /// answers changes to.
    pub fn render(&mut self, composition: &Composition) -> String {
        let body = self.build(composition);
        let html = body.html();
        self.last = Some(body);
        html
    }

    /// What changed on the page since the last render: each element to replace,
    /// outermost first. With nothing rendered before, it is the whole body.
    pub fn patch(&mut self, composition: &Composition) -> Vec<Patch> {
        let body = self.build(composition);
        let mut patches = Vec::new();
        let whole = match &self.last {
            Some(old) => !body.diff(old, &mut patches),
            None => true,
        };
        if whole {
            patches = vec![Patch {
                id: body.id.unwrap_or_default(),
                html: body.html(),
            }];
        }
        self.last = Some(body);
        patches
    }

    fn build(&mut self, composition: &Composition) -> Element {
        self.placed.clear();
        let mut body = Element::new("div");
        body.id = Some(self.id(""));
        body.attr("class", "tilo-view");
        let mut root = self.node(&composition.root, None, None);
        root.attr("data-root", "");
        body.children.push(root);
        for layer in &composition.layers {
            let mut layer = self.node(layer, None, None);
            layer.attr("data-layer", "");
            body.children.push(layer);
        }
        if let Some(focus) = &composition.focus {
            let id = self.id(focus);
            body.attr("data-focus", &id.to_string());
        }
        body
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one widget's element, read top to bottom as the page builds it"
    )]
    fn node(
        &mut self,
        node: &Composed,
        parent_flows: Option<bool>,
        placed_in: Option<[f64; 2]>,
    ) -> Element {
        let role = node.semantics.as_ref().map(|access| access.role);
        let mut element = Element::new(tag_of(node, role));
        element.id = Some(self.id(&node.path));
        let mut css = String::new();
        // Its place in its parent: the browser lays it out from its own style.
        if let Some(layout) = &node.layout {
            if node.children.is_empty() && node.own.is_some() {
                css.push_str(&sizing(layout));
            } else {
                css.push_str(layout);
            }
        } else if !node.children.is_empty() {
            css.push_str("display:flex;flex-direction:column;");
        }
        if parent_flows == Some(false) || parent_flows.is_none() && node.layout.is_none() {
            css.push_str("min-width:0;");
        }
        // A canvas's child stands where the canvas placed it, from the canvas's corner, at the
        // size it gave it: after its own style, which sizes it for a parent that flows it.
        if let Some([x, y]) = placed_in {
            let [at_x, at_y, width, height] = node.rect;
            let _ = write!(
                css,
                "position:absolute;left:{}px;top:{}px;width:{}px;height:{}px;\
                 min-width:0;min-height:0;max-width:none;max-height:none;flex:none;",
                at_x - x,
                at_y - y,
                width,
                height
            );
        }
        if !node.flows && !node.places && !node.children.is_empty() && node.own.is_none() {
            self.placed.push(node.path.clone());
        }
        if node.places {
            // What it places stands at points of its own, past its edges too: the box scrolls
            // to them.
            if placed_in.is_none() {
                css.push_str("position:relative;");
            }
            css.push_str("overflow:auto;");
        } else if node.scroll.is_some() {
            css.push_str("overflow-y:auto;overflow-x:hidden;overscroll-behavior:contain;");
        } else if node.clip.is_some() {
            css.push_str("overflow:clip;");
        }
        if let Some(ground) = &node.ground {
            if let Some(fill) = ground.fill {
                let _ = write!(css, "background:{};", rgba(fill));
            }
            let mut shadows = Vec::new();
            if node.floats {
                shadows.push(FLOATING_SHADOW.to_owned());
            }
            if let Some(edge) = ground.edge {
                shadows.push(format!("inset 0 0 0 1px {}", rgba(edge)));
            }
            if !shadows.is_empty() {
                let _ = write!(css, "box-shadow:{};", shadows.join(","));
            }
            if ground.radius > 0.0 {
                let _ = write!(css, "border-radius:{}px;", ground.radius);
            }
            // The ground stands in from the widget's edges, and what it holds with it.
            let [x, y] = ground.inset;
            if x > 0.0 || y > 0.0 {
                let _ = write!(css, "margin:{y}px {x}px;");
            }
        }
        match &node.own {
            Some(Own::Text(look)) => css.push_str(&text_css(look)),
            Some(Own::Face(face)) => css.push_str(&face_css(face)),
            Some(Own::Control { controls }) => {
                let captioned = controls.iter().any(|control| {
                    matches!(
                        control,
                        Control::Check {
                            style: CheckStyle::Switch,
                            label: Some(_),
                            ..
                        }
                    )
                });
                css.push_str(if captioned {
                    "display:flex;align-items:center;justify-content:space-between;gap:8px;min-width:0;"
                } else {
                    "display:flex;align-items:center;flex-wrap:wrap;gap:8px;min-width:0;"
                });
            }
            Some(Own::Field { look, inset, .. }) => {
                css.push_str(&text_style(look));
                let _ = write!(
                    css,
                    "margin:0;padding:{}px {}px;border:0;outline:none;font-family:inherit;resize:none;",
                    inset[1], inset[0]
                );
                if node.ground.is_none() {
                    css.push_str("background:transparent;");
                }
            }
            _ => {}
        }
        // A surface it lays under its children (a sidebar's) is its ground.
        if let Some((_, surface)) = node.surfaces.first() {
            let _ = write!(css, "background:var(--tilo-{surface},var(--tilo-sidebar));");
        }
        if !css.is_empty() {
            element.attr("style", &css);
        }
        if let Some(access) = &node.semantics {
            semantics_attrs(&mut element, node, access);
        }
        element.attr("data-w", &node.widget);
        if element.tag == "button" {
            element.attr("type", "button");
        }
        for (name, value) in &node.attrs {
            element.attr(name, value);
        }
        if acts(node) {
            element.attr("data-a", "activate");
            if let Some(target) = node
                .semantics
                .as_ref()
                .and_then(|a| a.action_target.as_ref())
            {
                element.attr("data-t", target);
            }
        }
        if let Some(place) = &node.well {
            element.attr("data-well", place);
            if let Some(shown) = self.wells.get(place) {
                element.own.clone_from(shown);
            }
        }
        let path = node.path.clone();
        match &node.own {
            Some(Own::Text(look)) => element.own = escape(&look.text),
            Some(Own::Field {
                value, placeholder, ..
            }) => {
                if let Some(placeholder) = placeholder {
                    element.attr("placeholder", placeholder);
                }
                element.attr("data-a", "value");
                if element.tag == "textarea" {
                    element.attr("rows", "1");
                    element.own = escape(value);
                } else {
                    element.attr("value", value);
                }
            }
            Some(Own::Blocks { blocks, edges }) => {
                let mut body = Element::new("div");
                body.attr("class", "tilo-doc");
                body.attr(
                    "style",
                    &format!(
                        "display:flex;flex-direction:column;padding-left:{}px;--tilo-list:{}px;--tilo-step:{}px;--tilo-quote:{}px;",
                        edges.text, edges.list, edges.indent, edges.quote
                    ),
                );
                for block in blocks {
                    body.children.push(self.block(&path, block));
                }
                element.children.push(body);
            }
            // Its picture, under what the page sets over it; a reader hears the element it fills.
            Some(Own::Painted {
                picture: Some(picture),
                ..
            }) => {
                let mut island = Element::new("img");
                island.id = Some(self.id(&format!("{path}#picture")));
                island.attr("src", picture);
                island.attr("alt", "");
                island.attr("style", "display:block;width:100%;height:100%;");
                element.children.push(island);
            }
            Some(Own::Painted { picture: None, .. }) | None => {}
            Some(Own::Face(face)) => element.own = face_html(face),
            Some(Own::Control { controls }) => {
                let target = node
                    .semantics
                    .as_ref()
                    .and_then(|access| access.action_target.clone());
                let disabled = node
                    .semantics
                    .as_ref()
                    .is_some_and(|access| access.state.disabled);
                let name = node
                    .semantics
                    .as_ref()
                    .and_then(|access| access.name.clone());
                for (index, control) in controls.iter().enumerate() {
                    let key = format!("{path}#control:{index}");
                    self.control(
                        &mut element,
                        &key,
                        control,
                        target.as_deref(),
                        name.as_deref(),
                        disabled,
                    );
                }
            }
        }
        let flows = node.flows;
        let corner = node.places.then_some([node.rect[0], node.rect[1]]);
        for child in &node.children {
            if child.rect[2] <= 0.0 || child.rect[3] <= 0.0 {
                continue;
            }
            element.children.push(self.node(child, Some(flows), corner));
        }
        // What a view draws itself, a page has no paint for: a control whose
        // own paint is all it shows still says its name, which the
        // accessibility tree already carries. Otherwise an empty notebook is
        // an empty page, and its offer of a first note cannot be pressed.
        if element.own.is_empty()
            && element.children.is_empty()
            && matches!(element.tag, "button" | "a")
        {
            if let Some(name) = node
                .semantics
                .as_ref()
                .and_then(|access| access.name.as_deref())
                .filter(|name| !name.trim().is_empty())
            {
                element.own = escape(name);
            }
        }
        element
    }

    /// A document block: its own element, keyed so typing in one block replaces
    /// that block alone.
    fn block(&mut self, editor: &str, block: &DocBlock) -> Element {
        let tag = match block.kind {
            "heading" => match block.level {
                0 | 1 => "h1",
                2 => "h2",
                _ => "h3",
            },
            "quote" => "blockquote",
            "code" => "pre",
            "divider" => "hr",
            "bullet" | "numbered" | "check" => "li",
            _ => "p",
        };
        let mut item = Element::new(tag);
        item.id = Some(self.id(&format!("{editor}#block:{}", block.id)));
        let mut class = format!("tilo-block tilo-{}", block.kind);
        if block.soft_break {
            class.push_str(" tilo-soft");
        }
        if block.done {
            class.push_str(" tilo-done");
        }
        item.attr("class", &class);
        item.attr(
            "style",
            &format!(
                "--tilo-indent:{};margin-top:{}px;font-size:{}px;line-height:{};font-weight:{};",
                block.level, block.gap, block.size, block.line_height, block.weight
            ),
        );
        if let Some(ordinal) = block.ordinal {
            item.attr("data-ordinal", &ordinal.to_string());
        }
        item.attr("data-block", &block.id);
        if block.kind == "divider" {
            return item;
        }
        if block.editable {
            item.attr("data-a", "edit");
            item.attr("data-t", &format!("notes.block-text:{}", block.id));
        }
        let mut own = String::new();
        if block.kind == "check" {
            let _ = write!(
                own,
                "<input type=\"checkbox\" class=\"tilo-check\" data-a=\"toggle\" data-t=\"notes.block-check:{}\"{}>",
                escape(&block.id),
                if block.done { " checked" } else { "" }
            );
        }
        own.push_str(&inline(block));
        item.own = own;
        item
    }

    fn control(
        &mut self,
        element: &mut Element,
        key: &str,
        control: &Control,
        target: Option<&str>,
        name: Option<&str>,
        disabled: bool,
    ) {
        match control {
            Control::Link { href, label } => {
                element.tag = "a";
                element.attr("class", "tilo-link");
                if let Some(href) = href {
                    element.attr("href", href);
                }
                element.own.push_str(&words(label));
                element.attr("data-a", "activate");
                if let Some(target) = target {
                    element.attr("data-t", target);
                }
            }
            Control::Check {
                style,
                checked,
                mixed,
                label,
            } => {
                let mut input = Element::new("input");
                input.id = Some(self.id(&format!("{key}:input")));
                input.attr(
                    "type",
                    if *style == CheckStyle::Radio {
                        "radio"
                    } else {
                        "checkbox"
                    },
                );
                if *style == CheckStyle::Switch {
                    input.attr("role", "switch");
                }
                if *checked {
                    input.flag("checked");
                }
                if *mixed {
                    input.attr("aria-checked", "mixed");
                }
                if disabled {
                    input.flag("disabled");
                }
                if label.is_none()
                    && let Some(name) = name
                {
                    input.attr("aria-label", name);
                }
                act(&mut input, "toggle", target, None);
                let label = label.as_ref().map(|label| {
                    let mut span = Element::new("span");
                    span.own = words(label);
                    span
                });
                match (label, style) {
                    (Some(label), CheckStyle::Switch) => {
                        element.children.push(label);
                        element.children.push(input);
                    }
                    (label, _) => {
                        element.children.push(input);
                        element.children.extend(label);
                    }
                }
            }
            Control::Range {
                value,
                min,
                max,
                step,
                text,
                shown,
            } => {
                let mut input = Element::new("input");
                input.id = Some(self.id(&format!("{key}:input")));
                input.attr("type", "range");
                input.attr("min", &min.to_string());
                input.attr("max", &max.to_string());
                input.attr(
                    "step",
                    &step.map_or_else(|| "any".to_owned(), |s| s.to_string()),
                );
                input.attr("value", &value.to_string());
                if let Some(name) = name {
                    input.attr("aria-label", name);
                }
                if let Some(text) = text {
                    input.attr("aria-valuetext", text);
                }
                if disabled {
                    input.flag("disabled");
                }
                let span = max - min;
                let along = if span > 0.0 {
                    ((value - min) / span * 100.0).clamp(0.0, 100.0)
                } else {
                    0.0
                };
                input.attr("style", &format!("--tilo-along:{along}%;"));
                act(&mut input, "value", target, None);
                element.children.push(input);
                if let Some(text) = text
                    && *shown
                {
                    let mut said = Element::new("span");
                    said.attr("class", "tilo-said");
                    said.attr("aria-hidden", "true");
                    said.own = escape(text);
                    element.children.push(said);
                }
            }
            Control::Entry {
                input: kind,
                target: field_target,
                text,
                placeholder,
                min,
                max,
                step,
                steppers,
                increase,
                decrease,
                secret,
            } => {
                let mut input = Element::new("input");
                input.id = Some(self.id(&format!("{key}:input")));
                let kind_name = match kind {
                    _ if *secret => "password",
                    InputKind::Number => "text",
                    InputKind::Date => "date",
                    InputKind::Time => "time",
                    InputKind::Colour => "color",
                    InputKind::Search => "search",
                };
                input.attr("type", kind_name);
                if *kind == InputKind::Number {
                    input.attr("role", "spinbutton");
                    input.attr("inputmode", "decimal");
                }
                if !placeholder.is_empty() {
                    input.attr("placeholder", placeholder);
                }
                if *secret {
                    input.attr("aria-label", placeholder);
                } else if let Some(name) = name {
                    input.attr("aria-label", name);
                }
                for (attr, value) in [("min", min), ("max", max), ("step", step)] {
                    if let Some(value) = value {
                        input.attr(attr, &value.to_string());
                    }
                }
                if !*secret {
                    input.attr("value", text);
                }
                if disabled {
                    input.flag("disabled");
                }
                act(
                    &mut input,
                    "value",
                    field_target.as_deref().or(target),
                    None,
                );
                let mut box_ = Element::new("span");
                box_.attr("class", "tilo-entry");
                if *steppers {
                    for (sign, label, part) in [(-1.0, decrease, "down"), (1.0, increase, "up")] {
                        let mut button = Element::new("button");
                        button.id = Some(self.id(&format!("{key}:{part}")));
                        button.attr("type", "button");
                        button.attr("class", "tilo-step");
                        if let Some(label) = label {
                            button.attr("aria-label", label);
                        }
                        button.attr("data-by", &(sign * step.unwrap_or(1.0)).to_string());
                        if disabled {
                            button.flag("disabled");
                        }
                        act(&mut button, "step", target, None);
                        box_.children.push(button);
                    }
                    box_.children.insert(1, input);
                } else {
                    box_.children.push(input);
                }
                element.children.push(box_);
            }
            Control::Select {
                current,
                placeholder,
                options,
                open,
                icon,
                chevron,
                ..
            } => {
                let mut button = Element::new("button");
                button.id = Some(self.id(&format!("{key}:current")));
                button.attr("type", "button");
                button.attr("role", "combobox");
                button.attr("aria-haspopup", "listbox");
                button.attr("aria-expanded", if *open { "true" } else { "false" });
                if disabled {
                    button.flag("disabled");
                }
                act(&mut button, "activate", target, None);
                let mut own = String::new();
                if let Some(icon) = icon {
                    own.push_str(&glyph(icon));
                }
                match current {
                    Some(current) => own.push_str(&words(current)),
                    None => {
                        let _ = write!(
                            own,
                            "<span class=\"tilo-quiet\">{}</span>",
                            escape(placeholder)
                        );
                    }
                }
                if let Some(chevron) = chevron {
                    own.push_str("<span class=\"tilo-chevron\">");
                    own.push_str(&glyph(chevron));
                    own.push_str("</span>");
                }
                button.own = own;
                let mut box_ = Element::new("div");
                box_.attr("class", "tilo-select");
                box_.children.push(button);
                if *open && !options.is_empty() {
                    let mut list = Element::new("ul");
                    list.attr("role", "listbox");
                    for (row, option) in options.iter().enumerate() {
                        list.children.push(self.option(
                            &format!("{key}:option:{row}"),
                            option,
                            target,
                            "option",
                        ));
                    }
                    box_.children.push(list);
                }
                element.children.push(box_);
            }
            Control::Choices {
                list,
                item,
                label,
                options,
            } => {
                let mut box_ = Element::new("div");
                box_.attr("role", list);
                box_.attr("class", "tilo-choices");
                if let Some(label) = label {
                    box_.own = words(label);
                }
                for (row, option) in options.iter().enumerate() {
                    box_.children.push(self.option(
                        &format!("{key}:choice:{row}"),
                        option,
                        target,
                        item,
                    ));
                }
                element.children.push(box_);
            }
        }
    }

    /// One choice of a control: an option of a chooser's list, a tab, a menu row.
    fn option(
        &mut self,
        key: &str,
        option: &ControlOption,
        fallback: Option<&str>,
        role: &str,
    ) -> Element {
        let mut item = Element::new(if role == "option" { "li" } else { "button" });
        item.id = Some(self.id(key));
        if item.tag == "button" {
            item.attr("type", "button");
        }
        item.attr("role", role);
        if role == "radio" {
            item.attr(
                "aria-checked",
                if option.selected { "true" } else { "false" },
            );
        } else {
            item.attr(
                "aria-selected",
                if option.selected { "true" } else { "false" },
            );
        }
        if option.disabled {
            item.attr("aria-disabled", "true");
        }
        let icon_only = role == "button" && option.icon.is_some();
        let swatch = role == "radio" && option.colour.is_some();
        if icon_only || swatch {
            item.attr("aria-label", &option.label);
            item.attr("title", &option.label);
        }
        item.attr(
            "class",
            if swatch {
                "tilo-choice tilo-swatch"
            } else if icon_only {
                "tilo-choice tilo-tool"
            } else {
                "tilo-choice"
            },
        );
        let mut own = String::new();
        if let Some(colour) = option.colour {
            let _ = write!(
                own,
                "<span class=\"tilo-colour\" style=\"background:{}\"></span>",
                rgba(colour)
            );
        }
        if let Some(icon) = &option.icon {
            own.push_str(&glyph(icon));
        }
        if !icon_only && !swatch {
            let _ = write!(
                own,
                "<span class=\"tilo-label\">{}</span>",
                escape(&option.label)
            );
        }
        if let Some(detail) = &option.detail {
            let _ = write!(own, "<span class=\"tilo-detail\">{}</span>", escape(detail));
        }
        if option.dot {
            own.push_str("<span class=\"tilo-dot\" aria-hidden=\"true\"></span>");
        }
        item.own = own;
        match &option.value {
            Some(value) => act(&mut item, "value", fallback, Some(value)),
            None => act(
                &mut item,
                "activate",
                option.target.as_deref().or(fallback),
                None,
            ),
        }
        item
    }
}

/// `element` carries the semantic action `action` on `target`, setting `value`.
fn act(element: &mut Element, action: &str, target: Option<&str>, value: Option<&str>) {
    element.attr("data-a", action);
    if let Some(target) = target {
        element.attr("data-t", target);
    }
    if let Some(value) = value {
        element.attr("data-v", value);
    }
}

/// Whether a press on the widget's element is its activation: a button, a row, a
/// link or a tab it composes itself, not a control the page gives its own element.
fn acts(node: &Composed) -> bool {
    if matches!(node.own, Some(Own::Control { .. } | Own::Field { .. })) {
        return false;
    }
    let Some(access) = &node.semantics else {
        return false;
    };
    if access.state.disabled {
        return false;
    }
    access.action_target.is_some()
        || matches!(
            access.role,
            Role::Button
                | Role::ToggleButton
                | Role::Link
                | Role::ListItem
                | Role::ListBoxOption
                | Role::MenuItem
                | Role::Tab
                | Role::TreeItem
                | Role::Checkbox
                | Role::Switch
                | Role::RadioButton
        )
}

fn tag_of(node: &Composed, role: Option<Role>) -> &'static str {
    if let Some(tag) = node.tag {
        return tag;
    }
    match (&node.own, role) {
        (
            Some(Own::Field {
                multiline: true, ..
            }),
            _,
        ) => "textarea",
        (Some(Own::Field { .. }), _) => "input",
        (Some(Own::Text(_)), Some(Role::Heading { level })) => match level {
            0 | 1 => "h1",
            2 => "h2",
            3 => "h3",
            4 => "h4",
            5 => "h5",
            _ => "h6",
        },
        (_, Some(Role::Button | Role::ToggleButton)) if node.children.is_empty() => "button",
        (_, Some(Role::Link)) => "a",
        (_, Some(Role::Document)) => "article",
        (_, Some(Role::Paragraph)) => "p",
        (_, Some(Role::Dialog)) => "dialog",
        (Some(Own::Text(_)), _) => "span",
        (_, Some(Role::Group))
            if node
                .semantics
                .as_ref()
                .is_some_and(|access| access.name.is_some()) =>
        {
            "section"
        }
        _ => "div",
    }
}

fn aria_role(role: Role) -> Option<&'static str> {
    Some(match role {
        Role::Toolbar => "toolbar",
        Role::TabBar => "tablist",
        Role::Tab => "tab",
        Role::TabPanel => "tabpanel",
        Role::List => "list",
        Role::ListItem => "listitem",
        Role::Table => "table",
        Role::Row => "row",
        Role::Cell => "cell",
        Role::Tree => "tree",
        Role::TreeItem => "treeitem",
        Role::Menu => "menu",
        Role::MenuItem => "menuitem",
        Role::ListBox => "listbox",
        Role::ListBoxOption => "option",
        Role::Checkbox => "checkbox",
        Role::RadioButton => "radio",
        Role::Switch => "switch",
        Role::Slider => "slider",
        Role::Separator => "separator",
        Role::ComboBox => "combobox",
        Role::ProgressBar => "progressbar",
        Role::Tooltip => "tooltip",
        Role::Image => "img",
        Role::Dialog => "dialog",
        Role::Button | Role::ToggleButton => "button",
        Role::Link => "link",
        _ => return None,
    })
}

/// The element's role, name and state, for assistive technology and for the
/// page's own find, focus and selection.
fn semantics_attrs(element: &mut Element, node: &Composed, access: &AccessNode) {
    let native = |role: &str| match role {
        "button" => element_is(element, "button"),
        "link" => element_is(element, "a"),
        "listitem" => element_is(element, "li"),
        "dialog" => element_is(element, "dialog"),
        _ => false,
    };
    let role = aria_role(access.role).filter(|role| !native(role));
    let live = access.live.map(|live| match live {
        crate::a11y::Politeness::Assertive => "alert",
        crate::a11y::Politeness::Polite => "status",
    });
    let role = match (live, role) {
        (Some(live), None | Some("group")) => Some(live),
        (_, role) => role,
    };
    if !matches!(node.own, Some(Own::Control { .. }))
        && let Some(role) = role
    {
        element.attr("role", role);
    }
    let visible_text = matches!(
        node.own,
        Some(Own::Text(_) | Own::Field { .. } | Own::Blocks { .. })
    ) || matches!(&node.own, Some(Own::Face(face))
        if face.label.as_ref().is_some_and(|label| Some(&label.text) == access.name.as_ref()));
    if let Some(name) = &access.name
        && (!visible_text || matches!(node.own, Some(Own::Field { .. })))
    {
        element.attr("aria-label", name);
    }
    let state = &access.state;
    if state.selected {
        element.attr("aria-selected", "true");
    }
    if access.role == Role::ToggleButton {
        element.attr(
            "aria-pressed",
            if state.checked == Some(true) {
                "true"
            } else {
                "false"
            },
        );
    }
    if let Some(expanded) = state.expanded {
        element.attr("aria-expanded", if expanded { "true" } else { "false" });
    }
    if state.disabled {
        element.attr("aria-disabled", "true");
    }
    if state.busy {
        element.attr("aria-busy", "true");
    }
    if access.role == Role::Dialog && state.modal {
        element.attr("aria-modal", "true");
    }
    if matches!(
        access.role,
        Role::Checkbox | Role::Switch | Role::RadioButton
    ) && !matches!(node.own, Some(Own::Control { .. }))
    {
        if state.mixed {
            element.attr("aria-checked", "mixed");
        } else if let Some(checked) = state.checked {
            element.attr("aria-checked", if checked { "true" } else { "false" });
        }
    }
    if let Some(level) = access.level
        && access.role == Role::TreeItem
    {
        element.attr("aria-level", &level.to_string());
    }
}

fn element_is(element: &Element, tag: &str) -> bool {
    element.tag == tag
}

/// A leaf's own size from its layout node's style: what it asks of its parent,
/// never how the parent lays it out (a leaf has nothing to lay out).
fn sizing(layout: &str) -> String {
    layout
        .split(';')
        .filter(|declaration| {
            let name = declaration.split(':').next().unwrap_or_default();
            matches!(
                name,
                "width"
                    | "height"
                    | "min-width"
                    | "min-height"
                    | "max-width"
                    | "max-height"
                    | "flex-grow"
                    | "flex-shrink"
                    | "flex-basis"
                    | "flex"
                    | "margin"
                    | "margin-top"
                    | "margin-right"
                    | "margin-bottom"
                    | "margin-left"
                    | "align-self"
                    | "padding"
            )
        })
        .map(|declaration| format!("{declaration};"))
        .collect()
}

fn rgba(color: [u8; 4]) -> String {
    format!(
        "rgba({},{},{},{:.3})",
        color[0],
        color[1],
        color[2],
        f32::from(color[3]) / 255.0
    )
}

fn text_style(look: &TextLook) -> String {
    let mut css = format!(
        "font-size:{}px;line-height:{};font-weight:{};",
        look.size, look.line_height, look.weight
    );
    if look.italic {
        css.push_str("font-style:italic;");
    }
    if let Some(color) = look.color {
        let _ = write!(css, "color:{};", rgba(color));
    }
    if look.mono {
        css.push_str("font-family:var(--tilo-mono);");
    }
    if look.struck {
        css.push_str("text-decoration:line-through;");
    }
    if look.align != "start" {
        let _ = write!(css, "text-align:{};", look.align);
    }
    css
}

/// Words as the widget sets them: one line cut short with an ellipsis, or as
/// many lines as it allows.
fn text_css(look: &TextLook) -> String {
    let fit = match look.max_lines {
        Some(1) => "min-width:0;white-space:pre;overflow:hidden;text-overflow:ellipsis;".to_owned(),
        Some(lines) => format!(
            "min-width:0;display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:{lines};overflow:hidden;white-space:pre-wrap;overflow-wrap:break-word;"
        ),
        None => "white-space:pre-wrap;overflow-wrap:break-word;".to_owned(),
    };
    text_style(look) + &fit
}

fn words(look: &TextLook) -> String {
    let fit = if look.max_lines.is_some_and(|lines| lines > 1) {
        "tilo-lines"
    } else {
        "tilo-line"
    };
    format!(
        "<span class=\"{fit}\" style=\"{}\">{}</span>",
        text_style(look),
        escape(&look.text)
    )
}

fn face_css(face: &Face) -> String {
    let mut css = format!(
        "display:flex;align-items:center;justify-content:{};gap:{}px;padding:0 {}px;border-radius:{}px;white-space:nowrap;",
        if face.spread {
            "space-between"
        } else {
            "center"
        },
        face.gap,
        face.pad,
        face.radius
    );
    if let Some(fill) = face.fill {
        let _ = write!(css, "background:{};", rgba(fill));
    }
    if let Some(edge) = face.edge {
        let _ = write!(css, "box-shadow:inset 0 0 0 1px {};", rgba(edge));
    }
    if let Some(ink) = face
        .label
        .as_ref()
        .and_then(|label| label.color)
        .or(face.icon.as_ref().map(|icon| icon.color))
    {
        let _ = write!(css, "color:{};", rgba(ink));
    }
    css
}

fn face_html(face: &Face) -> String {
    let mut out = String::new();
    if let Some(icon) = &face.icon {
        out.push_str(&glyph(icon));
    }
    if face.label.is_some() || face.detail.is_some() {
        let _ = write!(
            out,
            "<span class=\"tilo-face-words\" style=\"gap:{}px;{}\">",
            face.gap * 2.0,
            if face.spread {
                "flex:1;justify-content:space-between;"
            } else {
                ""
            }
        );
        if let Some(label) = &face.label {
            out.push_str(&words(label));
        }
        if let Some(detail) = &face.detail {
            let _ = write!(
                out,
                "<span style=\"{}flex:none;white-space:pre;\">{}</span>",
                text_style(detail),
                escape(&detail.text)
            );
        }
        out.push_str("</span>");
    }
    if let Some(trailing) = &face.trailing {
        out.push_str(&glyph(trailing));
    }
    out
}

/// An icon: its outlines as an inline SVG in its colour, its stroke as wide on
/// screen at every size.
fn glyph(icon: &Glyph) -> String {
    let mut out = format!(
        "<svg class=\"tilo-icon\" aria-hidden=\"true\" viewBox=\"0 0 24 24\" width=\"{size}\" height=\"{size}\" style=\"color:{color}\" fill=\"{fill}\" stroke=\"currentColor\" stroke-width=\"{stroke}\" stroke-linecap=\"round\" stroke-linejoin=\"round\">",
        size = icon.size,
        color = rgba(icon.color),
        fill = if icon.filled { "currentColor" } else { "none" },
        stroke = icon.stroke * 24.0 / icon.size.max(1.0),
    );
    for path in &icon.paths {
        let _ = write!(out, "<path d=\"{}\"/>", escape(path));
    }
    out.push_str("</svg>");
    out
}

/// A block's text as pieces of its marks. The caret and selection are the page's
/// own while the person edits, so they are not drawn into the text.
fn inline(block: &DocBlock) -> String {
    let text = &block.text;
    if text.is_empty() {
        return "<br>".to_owned();
    }
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for mark in &block.marks {
        cuts.push(mark.start.min(text.len()));
        cuts.push(mark.end.min(text.len()));
    }
    cuts.retain(|cut| text.is_char_boundary(*cut));
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = String::new();
    for pair in cuts.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let mut piece = escape(text.get(from..to).unwrap_or_default());
        for mark in &block.marks {
            if mark.start > from || mark.end < to {
                continue;
            }
            let tag = match mark.mark {
                "bold" | "mention" => "strong",
                "italic" => "em",
                "code" => "code",
                "underline" => "u",
                "strike" => "s",
                "link" => "a",
                _ => "span",
            };
            let mut open = format!("<{tag}");
            if mark.mark == "link"
                && let Some(href) = &mark.href
            {
                let _ = write!(open, " href=\"{}\"", escape(href));
            }
            if mark.mark == "mention" {
                open.push_str(" class=\"tilo-mention\"");
            }
            piece = format!("{open}>{piece}</{tag}>");
        }
        out.push_str(&piece);
    }
    out
}

/// `text` safe in an element's content and in a quoted attribute.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// The theme as the page's custom properties (`--tilo-*`) and the
/// `color-scheme` the browser draws its own parts in.
#[must_use]
pub fn theme_css(theme: &crate::Theme) -> String {
    let colors = &theme.tokens.colors;
    let css = |color: vello::peniko::Color| rgba(super::rgba8(color));
    let look = if theme.document.is_dark() {
        "dark"
    } else {
        "light"
    };
    // The pointer's tints, as the app answers it on each ground it stands on.
    let tint = |ground, hover, pressed| {
        css(crate::chrome::selection_tier_on(
            theme,
            crate::chrome::SelectionState {
                hover,
                pressed,
                ..crate::chrome::SelectionState::default()
            },
            ground,
        )
        .fill)
    };
    let (content, sidebar) = (colors.surfaces.content, colors.surfaces.sidebar);
    format!(
        ":root{{--tilo-hover:{};--tilo-press:{};}}[data-pane=sidebar]{{--tilo-hover:{};--tilo-press:{};}}:root{{color-scheme:{look};--tilo-ground:{};--tilo-sidebar:{};--tilo-raised:{};--tilo-overlay:{};--tilo-text:{};--tilo-text-dim:{};--tilo-text-tertiary:{};--tilo-border:{};--tilo-focus:{};--tilo-accent:{};--tilo-on-accent:{};--tilo-radius:{:.1}px;}}",
        tint(content, true, false),
        tint(content, false, true),
        tint(sidebar, true, false),
        tint(sidebar, false, true),
        css(colors.surfaces.content),
        css(colors.surfaces.sidebar),
        css(colors.surfaces.raised),
        css(colors.surfaces.overlay),
        css(theme.text_primary),
        css(theme.text_dim),
        css(theme.text_tertiary),
        css(theme.border_standard),
        css(theme.border_focus),
        css(colors.intent.accent),
        css(colors.intent.on_accent),
        theme.radius_standard,
    )
}

/// A whole page: `title`, the theme's properties and the page's stylesheet in
/// its head, `body` (what [`Page::render`] answered) and `tail` (its script) in
/// its body.
#[must_use]
pub fn document(title: &str, theme: &crate::Theme, body: &str, tail: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{}{PAGE_CSS}</style></head><body>{body}{tail}</body></html>",
        escape(title),
        theme_css(theme),
    )
}

/// The page's own stylesheet: what every rendered view shares beyond what each
/// element's style says.
pub const PAGE_CSS: &str = include_str!("page.css");

const FLOATING_SHADOW: &str = "0 1px 3px rgba(0,0,0,.07),0 8px 22px rgba(0,0,0,.09)";

#[cfg(test)]
mod tests;
