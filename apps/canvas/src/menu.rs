//! The canvas's context menu: what a right-click offers for what is
//! selected, the area selected, or nothing at all. Every entry is a
//! command the artwork already has: the keys' commands go through the
//! canvas's own key handling (and show those keys), the rest are the
//! artwork's actions, the same ones the strip over the canvas makes.

use ui::framework::{Dispatcher, Live};
use ui::tilo_resource::Invoke;
use ui::vello::kurbo::Point;
use ui::widget::MenuEntry;
use ui::winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::actions::{Align, Arrange};
use crate::model::Artwork;
use crate::panels::shortcuts::keys_for;
use crate::selection::{Combine, Select, UseSelection};
use crate::tools::{CanvasKey, Tools};

/// The canvas with its context menu.
pub fn over(
    canvas: ui::widget::Element,
    tools: &Tools,
    live: &Live<Artwork>,
    dispatcher: Dispatcher<Artwork>,
) -> ui::widget::Element {
    let tools = tools.clone();
    let live = live.clone();
    Box::new(ui::widget::ContextMenuArea::new(
        canvas,
        move |_at: Option<Point>| entries(&tools, &live.get(), &dispatcher),
    ))
}

/// A command the canvas's keys already make: pressing them for the
/// person, so the menu and the keys can never differ.
fn keyed(tools: &Tools, label: &str, does: &str, key: Key, modifiers: ModifiersState) -> MenuEntry {
    let keys = tools.keys.clone();
    let text = match &key {
        Key::Character(ch) => Some(ch.to_string()),
        _ => None,
    };
    let entry = MenuEntry::new(lookup(label), move || {
        keys.update(|keys| {
            keys.push(CanvasKey {
                key: key.clone(),
                text: text.clone(),
                modifiers,
            });
        });
    });
    match keys_for(does) {
        Some(shortcut) => entry.with_shortcut(shortcut),
        None => entry,
    }
}

fn ctrl(tools: &Tools, label: &str, does: &str, ch: &str) -> MenuEntry {
    keyed(
        tools,
        label,
        does,
        Key::Character(ch.into()),
        ModifiersState::CONTROL,
    )
}

fn lookup(id: &str) -> String {
    ui::i18n::I18nManager::lookup_plain(id)
}

/// An entry that applies `action` to the artwork.
fn applying(dispatcher: &Dispatcher<Artwork>, label: &str, action: Invoke<Artwork>) -> MenuEntry {
    let dispatcher = dispatcher.clone();
    MenuEntry::new(lookup(label), move || dispatcher.submit(action.clone()))
}

/// What the menu offers now.
fn entries(tools: &Tools, art: &Artwork, dispatcher: &Dispatcher<Artwork>) -> Vec<MenuEntry> {
    let picked = tools.selection.get();
    let mut entries = Vec::new();
    if picked.is_empty() {
        entries.push(ctrl(
            tools,
            "canvas-context-paste",
            "canvas-keys-paste",
            "v",
        ));
        entries.push(ctrl(
            tools,
            "canvas-context-select-all",
            "canvas-keys-select-all",
            "a",
        ));
    } else {
        marks(tools, art, dispatcher, &picked, &mut entries);
    }
    if art.selection.is_some() {
        entries.push(MenuEntry::separator());
        area(tools, dispatcher, &mut entries);
    }
    // No rule at the end, nor two in a row where a group came out empty.
    let mut tidy: Vec<MenuEntry> = Vec::with_capacity(entries.len());
    for entry in entries {
        let rule = entry.item().label().is_none();
        if rule && tidy.last().is_none_or(|last| last.item().label().is_none()) {
            continue;
        }
        tidy.push(entry);
    }
    if tidy
        .last()
        .is_some_and(|last| last.item().label().is_none())
    {
        tidy.pop();
    }
    tidy
}

/// Cut, copy, paste, duplicate and delete; the order of the marks; groups;
/// lining them up; and making one shape of several.
fn marks(
    tools: &Tools,
    art: &Artwork,
    dispatcher: &Dispatcher<Artwork>,
    picked: &[String],
    entries: &mut Vec<MenuEntry>,
) {
    entries.push(ctrl(tools, "canvas-context-cut", "canvas-keys-cut", "x"));
    entries.push(ctrl(tools, "canvas-context-copy", "canvas-keys-copy", "c"));
    entries.push(ctrl(
        tools,
        "canvas-context-paste",
        "canvas-keys-paste",
        "v",
    ));
    entries.push(ctrl(
        tools,
        "canvas-context-duplicate",
        "canvas-keys-duplicate",
        "d",
    ));
    let delete = keyed(
        tools,
        "canvas-context-delete",
        "canvas-keys-delete",
        Key::Named(NamedKey::Delete),
        ModifiersState::empty(),
    );
    entries.push(delete);
    entries.push(MenuEntry::separator());
    for (label, to) in [
        ("canvas-context-front", "front"),
        ("canvas-context-forward", "forward"),
        ("canvas-context-backward", "backward"),
        ("canvas-context-back", "back"),
    ] {
        entries.push(applying(
            dispatcher,
            label,
            Artwork::ARRANGE.with(Arrange {
                ids: picked.to_vec(),
                to: to.into(),
                of: None,
            }),
        ));
    }
    entries.push(MenuEntry::separator());
    if picked.len() >= 2 {
        entries.push(ctrl(
            tools,
            "canvas-context-group",
            "canvas-keys-group",
            "g",
        ));
    }
    if crate::gesture::edit::ungroup(art, picked).is_some() {
        entries.push(keyed(
            tools,
            "canvas-context-ungroup",
            "canvas-keys-ungroup",
            Key::Character("g".into()),
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        ));
    }
    // Several line up among themselves; one mark's alignment on the
    // artwork stays in the strip over the canvas, keeping this menu short.
    if picked.len() < 2 {
        return;
    }
    entries.push(MenuEntry::separator());
    let to: Option<String> = None;
    for (label, edge) in [
        ("canvas-align-left", "left"),
        ("canvas-align-center", "center"),
        ("canvas-align-right", "right"),
        ("canvas-align-top", "top"),
        ("canvas-align-middle", "middle"),
        ("canvas-align-bottom", "bottom"),
    ] {
        entries.push(applying(
            dispatcher,
            label,
            Artwork::ALIGN.with(Align {
                ids: picked.to_vec(),
                edge: Some(edge.into()),
                distribute: None,
                to: to.clone(),
            }),
        ));
    }
    {
        entries.push(MenuEntry::separator());
        for op in ["union", "subtract", "intersect", "exclude"] {
            entries.push(applying(
                dispatcher,
                &format!("canvas-combine-{op}"),
                Artwork::COMBINE.with(Combine {
                    ids: picked.to_vec(),
                    op: op.into(),
                }),
            ));
        }
    }
}

/// The area selected: fill or outline it with the brush's paint, mask or
/// clear the chosen layer with it, invert it, let it go.
fn area(tools: &Tools, dispatcher: &Dispatcher<Artwork>, entries: &mut Vec<MenuEntry>) {
    let loaded = tools.loaded();
    for (label, what) in [
        ("canvas-context-fill", "fill"),
        ("canvas-context-outline", "stroke"),
        ("canvas-context-mask", "mask"),
        ("canvas-context-clear", "clear"),
    ] {
        let dispatcher = dispatcher.clone();
        let loaded = loaded.clone();
        let run = move || {
            dispatcher.submit(Artwork::USE_SELECTION.with(UseSelection {
                what: what.to_owned(),
                layer: loaded.layer(),
                colour: Some(loaded.paint()),
                ..UseSelection::default()
            }));
        };
        entries.push(if what == "clear" {
            MenuEntry::destructive(lookup(label), run)
        } else {
            MenuEntry::new(lookup(label), run)
        });
    }
    entries.push(applying(
        dispatcher,
        "canvas-context-invert",
        Artwork::SELECT.with(Select {
            invert: Some(true),
            ..Select::default()
        }),
    ));
    entries.push(applying(
        dispatcher,
        "canvas-area-none",
        Artwork::SELECT.with(Select {
            mode: Some("none".into()),
            ..Select::default()
        }),
    ));
}
