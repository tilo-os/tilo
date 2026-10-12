//! The layers panel: one row per layer, top first, with its picture, its
//! name (double-click to rename), its medium, and whether it shows and
//! holds still; under it, the chosen layer's opacity and order, and adding
//! a layer in any medium. Every control is one of the artwork's actions.

use std::cell::RefCell;
use std::rc::Rc;

use ui::LocalState;
use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::ButtonRole;
use ui::widget::{Badge, ChipStyle, ControlRow, EditableLabel, Tone};

use super::{caption, icon_button, percent};
use crate::model::Artwork;
use crate::tools::Tools;

/// A layer as its row shows it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "shown, held still and clipped are independent facts of a layer"
)]
#[derive(Clone, Default, PartialEq)]
struct LayerRow {
    id: String,
    name: String,
    group: String,
    medium: String,
    visible: bool,
    locked: bool,
    opacity: f64,
    /// Its mask, shown beside it in its row rather than as a row of its own.
    mask: Option<MaskRow>,
    clip: bool,
}

/// A layer's mask, as its layer's row shows it.
#[derive(Clone, Default, PartialEq)]
struct MaskRow {
    id: String,
    invert: bool,
    /// Stays put when the layer's marks move.
    unlinked: bool,
    /// Switched on: the layer shows through it.
    visible: bool,
}

/// The short name of a medium's engine, `plain` for vector art.
fn engine_label(art: &Artwork, medium: &str) -> String {
    let engine = crate::media::resolve(art, medium)
        .filter(|_| !medium.is_empty())
        .map(|medium| medium.engine);
    let id = match engine.as_deref() {
        Some("oil") => "canvas-medium-oil",
        Some("watercolour") => "canvas-medium-watercolour",
        _ => "canvas-medium-plain",
    };
    ui::i18n::I18nManager::lookup_plain(id)
}

/// The layers' rows, top first; a mask rides in its layer's row.
fn rows(art: &Artwork) -> Vec<LayerRow> {
    art.layers
        .iter()
        .rev()
        .filter(|layer| !layer.is_mask())
        .map(|layer| LayerRow {
            id: layer.id.clone(),
            name: layer.name.clone(),
            group: layer.group.clone(),
            medium: engine_label(art, &layer.medium),
            visible: layer.visible,
            locked: layer.locked,
            opacity: layer.opacity,
            mask: art.mask_of(&layer.id).map(|mask| MaskRow {
                id: mask.id.clone(),
                invert: mask.invert,
                unlinked: mask.unlinked,
                visible: mask.visible,
            }),
            clip: layer.clip,
        })
        .collect()
}

/// A layer's picture in its row, the artwork's shape at a glance.
const THUMB: (f32, f32) = (44.0, 33.0); // a picture's size, not spacing

/// How tall the list of layers grows before it scrolls.
const LIST: f32 = 5.0 * ui::tokens::Metrics::BASE.panel_row_height;

/// The panel: the layers, scrolling when there are many, and under them
/// the chosen layer's own controls, always in view.
pub fn panel(cx: &ViewCx<Artwork>, tools: &Tools) -> Element {
    let rows = cx.value(rows);
    let list = {
        let tools = tools.clone();
        let known: Rc<RefCell<Vec<String>>> = Rc::default();
        cx.switch(&rows, move |_cx, rows| {
            follow_new_layers(&known, rows, &tools.layer);
            layer_tree(rows, &tools)
        })
    };
    // The chosen layer's own controls, built again when another is chosen.
    let chosen_tools = tools.clone();
    let footer = cx.switch(&tools.layer, move |cx, id| {
        chosen_layer(cx, id, &chosen_tools)
    });
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(0.0)
            .push(Box::new(
                Column::new()
                    .with_intrinsic_height()
                    .with_vertical_padding(ui::tokens::Spacing::BASE.s1)
                    .with_horizontal_padding(
                        ui::tokens::Spacing::BASE.s2,
                        ui::tokens::Spacing::BASE.s2,
                    )
                    .push(list),
            ))
            .push(Box::new(ui::widget::Divider::horizontal()))
            .push(Box::new(
                Column::new()
                    .with_intrinsic_height()
                    .with_vertical_padding(ui::tokens::Spacing::BASE.s2)
                    .with_horizontal_padding(
                        ui::tokens::Spacing::BASE.s4,
                        ui::tokens::Spacing::BASE.s3,
                    )
                    .push(footer),
            )),
    )
}

/// Folder branches keep paint order: non-contiguous runs stay separate rather
/// than suggesting a different composite order. The shared tree owns collapse,
/// keyboard navigation, virtualisation and accessibility.
fn layer_tree(rows: &[LayerRow], tools: &Tools) -> Element {
    use ui::widget::{TreeNode, TreeRowDecoration, TreeView};
    let mut children: Vec<TreeNode<()>> = Vec::new();
    let mut targets = std::collections::HashMap::new();
    let mut media = std::collections::HashMap::new();
    let mut next = u64::try_from(rows.len()).unwrap_or(0).saturating_add(1);
    let mut previous = "";
    let mut folder = None;
    for (index, row) in rows.iter().enumerate() {
        let id = u64::try_from(index).unwrap_or(0).saturating_add(1);
        let leaf = TreeNode::leaf(id, &row.name, ());
        media.insert(id, row.medium.clone());
        targets.insert(id, row.id.clone());
        if row.group != previous {
            previous = &row.group;
            folder = if row.group.is_empty() {
                None
            } else {
                let at = children.len();
                children.push(TreeNode::branch(next, &row.group, (), Vec::new()));
                next = next.saturating_add(1);
                Some(at)
            };
        }
        if let Some(branch) = folder.and_then(|at| children.get_mut(at)) {
            branch.children.push(leaf);
        } else {
            children.push(leaf);
        }
    }
    let expanded = children
        .iter()
        .filter(|node| node.is_branch())
        .map(|node| node.id)
        .collect::<Vec<_>>();
    let selected = targets
        .iter()
        .find_map(|(node, id)| (*id == tools.layer.get()).then_some(*node));
    let chosen = tools.layer.clone();
    Box::new(
        TreeView::new(TreeNode::branch(0, "", (), children))
            .hide_root()
            .with_id("canvas-layer-tree")
            .with_row_decoration(move |_, row, _| {
                media
                    .get(&row.id)
                    .map_or_else(TreeRowDecoration::new, |medium| {
                        TreeRowDecoration::new()
                            .trailing(medium)
                            .description(medium)
                    })
            })
            .with_selected(selected)
            .with_expanded(expanded)
            .with_viewport(LIST, ui::tokens::Metrics::BASE.panel_row_height)
            .on_select(move |node| {
                if let Some(id) = targets.get(&node) {
                    chosen.set(id.clone());
                }
            }),
    )
}

/// New marks go on the chosen layer; a layer added since (by anyone) is
/// where the next mark goes, and a removed one's choice falls to the top.
fn follow_new_layers(known: &RefCell<Vec<String>>, rows: &[LayerRow], target: &LocalState<String>) {
    // A mask counts too: one just added is where the next mark goes, as a
    // person adding a mask means to paint it.
    let ids: Vec<String> = rows
        .iter()
        .flat_map(|row| {
            std::iter::once(row.id.clone()).chain(row.mask.as_ref().map(|mask| mask.id.clone()))
        })
        .collect();
    let mut known = known.borrow_mut();
    let added = ids.iter().find(|id| !known.contains(id)).cloned();
    let chosen = target.with(Clone::clone);
    let next = match added {
        Some(added) if !known.is_empty() => Some(added),
        _ if !ids.contains(&chosen) => ids.first().cloned(),
        _ => None,
    };
    if let Some(next) = next {
        target.set(next);
    }
    *known = ids;
}

/// A layer's picture, and its mask's beside it when it has one: each, with
/// a mask, choosing what the brush paints.
fn pictures(cx: &ViewCx<Artwork>, tools: &Tools, row: &LayerRow) -> Row {
    let thumb = |layer: &str, name: &str| -> Element {
        let mut picture = crate::thumb::Thumb::new(
            tools.renderer.clone(),
            Some(layer.to_owned()),
            THUMB.0,
            THUMB.1,
        );
        if row.mask.is_some() {
            picture = picture.ringed_while(&tools.layer, layer);
        }
        let picture = cx.surface(picture);
        if row.mask.is_none() {
            return picture;
        }
        // With a mask beside it, each picture chooses what is painted: the
        // layer's paint or its mask.
        let chosen = tools.layer.clone();
        let target = layer.to_owned();
        let choose = Button::new(&format!("canvas-paint-{layer}"), "")
            .role(ButtonRole::Plain)
            .with_accessible_name(name)
            .tooltip(name)
            .fill_width()
            .on_view(move || chosen.set(target.clone()));
        // The picture over its button: the button takes the press the
        // picture lets through, and the picture shows what it chooses.
        Box::new(
            Container::new()
                .with_no_padding()
                .with_size(THUMB.0, THUMB.1)
                .with_child(Box::new(
                    ui::widget::Stack::new()
                        .push(Box::new(choose))
                        .push(picture),
                )),
        )
    };
    let named = |id: &str| ui::i18n::I18nManager::format(id, &[("name", row.name.clone().into())]);
    let mut pictures = Row::new()
        .with_gap(ui::tokens::Spacing::BASE.s1)
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(thumb(&row.id, &named("canvas-layer-paint-name")));
    if let Some(mask) = &row.mask {
        pictures = pictures.push(thumb(&mask.id, &named("canvas-layer-paint-mask-name")));
    }
    pictures
}

fn layer_row(cx: &ViewCx<Artwork>, tools: &Tools, row: &LayerRow) -> Element {
    let pictures = pictures(cx, tools, row);
    let named = |id: &str| ui::i18n::I18nManager::format(id, &[("name", row.name.clone().into())]);
    let rename = Artwork::RENAME_LAYER.map({
        let id = row.id.clone();
        move |name: String| (id.clone(), name)
    });
    let renaming = LocalState::new(false);
    let name = cx.actions().bind(
        EditableLabel::new(&row.name)
            .with_text_role(TextRole::Body)
            .with_id(&format!("canvas-layer-name-{}", row.id))
            .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-layer-rename"))
            .renames_on_double_click()
            .begins_editing_on(&renaming)
            .filling(),
        rename,
    );
    let mut summary = Row::new()
        .with_gap(ui::tokens::Spacing::BASE.s2)
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(Box::new(
            Badge::text(row.medium.clone()).style(ChipStyle::Tinted(Tone::Neutral)),
        ));
    if row.opacity < 1.0 {
        summary = summary.push(caption(&percent(row.opacity)));
    }
    let text = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(0.0)
        .push(Box::new(name))
        .push(Box::new(summary));
    let (eye, eye_name) = if row.visible {
        (Icon::Eye, named("canvas-layer-hide-name"))
    } else {
        (Icon::EyeOff, named("canvas-layer-show-name"))
    };
    let (lock, lock_name) = if row.locked {
        (Icon::Lock, named("canvas-layer-unlock-name"))
    } else {
        (Icon::LockOpen, named("canvas-layer-lock-name"))
    };
    let content = Row::new()
        .with_padding(ui::tokens::Spacing::BASE.s2, ui::tokens::Spacing::BASE.s1)
        .with_gap(ui::tokens::Spacing::BASE.s3)
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(Box::new(pictures))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_min_width(0.0)
                .with_child(Box::new(text)),
        ))
        .push(Box::new(icon_button(
            cx,
            eye,
            &eye_name,
            Artwork::SET_LAYER_VISIBLE.with((row.id.clone(), !row.visible)),
        )))
        .push(Box::new(icon_button(
            cx,
            lock,
            &lock_name,
            Artwork::SET_LAYER_LOCKED.with((row.id.clone(), !row.locked)),
        )));
    let row_element: Element = Box::new(
        ControlRow::new(content)
            .choosing_any(
                &tools.layer,
                std::iter::once(row.id.clone())
                    .chain(row.mask.as_ref().map(|mask| mask.id.clone()))
                    .collect(),
            )
            .with_accessible_name(row.name.clone()),
    );
    let menu = row_menu(cx, tools, row, renaming);
    Box::new(ui::widget::ContextMenuArea::new(row_element, menu))
}

/// What a right-click on a layer's row offers: the row's own controls and
/// the chosen layer's, as commands with words, the layer chosen first.
fn row_menu(
    cx: &ViewCx<Artwork>,
    tools: &Tools,
    row: &LayerRow,
    renaming: LocalState<bool>,
) -> impl Fn(Option<ui::vello::kurbo::Point>) -> Vec<ui::widget::MenuEntry> + 'static {
    use ui::widget::MenuEntry;
    let dispatcher = cx.dispatcher();
    let live = cx.live().clone();
    let chosen = tools.layer.clone();
    let row = row.clone();
    move |_| {
        // The row the menu is about is the chosen one while it is open,
        // as a left click would choose it.
        let mine = row
            .mask
            .as_ref()
            .is_some_and(|mask| chosen.with(|id| *id == mask.id));
        if !mine && chosen.with(|id| *id != row.id) {
            chosen.set(row.id.clone());
        }
        let lookup = ui::i18n::I18nManager::lookup_plain;
        let named =
            |id: &str| ui::i18n::I18nManager::format(id, &[("name", row.name.clone().into())]);
        let invoke = |make: fn(&LayerRow) -> Invoke<Artwork>| {
            let dispatcher = dispatcher.clone();
            let row = row.clone();
            move || dispatcher.submit(make(&row))
        };
        let has_selection = live.get().selection.is_some();
        let renaming = renaming.clone();
        let mut entries = vec![
            MenuEntry::new(lookup("canvas-context-rename-layer"), move || {
                renaming.set(true);
            }),
            MenuEntry::separator(),
            MenuEntry::new(
                named(if row.visible {
                    "canvas-layer-hide-name"
                } else {
                    "canvas-layer-show-name"
                }),
                invoke(|row| Artwork::SET_LAYER_VISIBLE.with((row.id.clone(), !row.visible))),
            ),
            MenuEntry::new(
                named(if row.locked {
                    "canvas-layer-unlock-name"
                } else {
                    "canvas-layer-lock-name"
                }),
                invoke(|row| Artwork::SET_LAYER_LOCKED.with((row.id.clone(), !row.locked))),
            ),
            MenuEntry::separator(),
            MenuEntry::new(
                lookup("canvas-layer-up"),
                invoke(|row| Artwork::MOVE_LAYER.with((row.id.clone(), "forward".into()))),
            ),
            MenuEntry::new(
                lookup("canvas-layer-down"),
                invoke(|row| Artwork::MOVE_LAYER.with((row.id.clone(), "backward".into()))),
            ),
            MenuEntry::separator(),
        ];
        masking(&row, &dispatcher, has_selection, &mut entries);
        entries.push(MenuEntry::separator());
        entries.push(MenuEntry::destructive(
            named("canvas-layer-remove-name"),
            invoke(|row| Artwork::REMOVE_LAYER_BY_ID.with(row.id.clone())),
        ));
        entries
    }
}

/// A layer row's menu entries for its mask and clipping.
fn masking(
    row: &LayerRow,
    dispatcher: &ui::framework::Dispatcher<Artwork>,
    has_selection: bool,
    entries: &mut Vec<ui::widget::MenuEntry>,
) {
    use ui::widget::MenuEntry;
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let invoke = |make: fn(&LayerRow) -> Invoke<Artwork>| {
        let dispatcher = dispatcher.clone();
        let row = row.clone();
        move || dispatcher.submit(make(&row))
    };
    if row.mask.is_some() {
        entries.push(MenuEntry::new(
            lookup("canvas-layer-mask-remove"),
            invoke(|row| Artwork::SET_LAYER_MASK.with((row.id.clone(), "remove".into()))),
        ));
    } else {
        entries.push(MenuEntry::new(
            lookup("canvas-layer-mask-add"),
            invoke(|row| Artwork::SET_LAYER_MASK.with((row.id.clone(), "reveal".into()))),
        ));
    }
    // The selection as the layer's mask: what it shows of the layer.
    let dispatch = dispatcher.clone();
    let layer = row.id.clone();
    entries.push(
        MenuEntry::new(lookup("canvas-context-mask-selection"), move || {
            dispatch.submit(Artwork::USE_SELECTION.with(crate::selection::UseSelection {
                what: "mask".into(),
                layer: Some(layer.clone()),
                ..crate::selection::UseSelection::default()
            }));
        })
        .with_enabled(has_selection),
    );
    entries.push(MenuEntry::new(
        lookup(if row.clip {
            "canvas-context-unclip"
        } else {
            "canvas-command-layer-clip"
        }),
        invoke(|row| Artwork::SET_LAYER_CLIP.with((row.id.clone(), !row.clip))),
    ));
}

/// The chosen layer's opacity, medium and place among the layers.
fn chosen_layer(cx: &ViewCx<Artwork>, id: &str, tools: &Tools) -> Element {
    let Some(row) = cx.read(|art| {
        art.and_then(|art| {
            rows(art)
                .into_iter()
                .find(|row| row.id == id || row.mask.as_ref().is_some_and(|mask| mask.id == id))
        })
    }) else {
        return Box::new(Spacer::token(ui::tokens::SpacingStep::S0));
    };
    let opacity = {
        let id = row.id.clone();
        cx.value(move |art: &Artwork| art.layer(&id).map_or(1.0, |layer| layer.opacity))
    };
    let slider = cx.actions().bind(
        Slider::from_state(&opacity)
            .with_id("canvas-layer-opacity")
            .with_accessible_name(&ui::i18n::I18nManager::lookup_plain(
                "canvas-label-layer-opacity",
            ))
            .commits_on_release(),
        Artwork::SET_LAYER_OPACITY.map({
            let id = row.id.clone();
            move |value: f64| (id.clone(), (value * 100.0).round() / 100.0)
        }),
    );
    let label = {
        Text::dynamic(move || {
            ui::i18n::I18nManager::format(
                "canvas-layer-opacity-value",
                &[("percent", percent(opacity.with(|value| *value)).into())],
            )
        })
        .with_role(TextRole::Caption)
    };
    let order = |icon, name: &str, to: &str| {
        icon_button(
            cx,
            icon,
            &ui::i18n::I18nManager::lookup_plain(name),
            Artwork::MOVE_LAYER.with((row.id.clone(), to.to_owned())),
        )
    };
    let raise = order(Icon::ArrowUp, "canvas-layer-up", "forward");
    let lower = order(Icon::ArrowDown, "canvas-layer-down", "backward");
    let masking = mask_buttons(cx, &row);
    let remove = icon_button(
        cx,
        Icon::Trash2,
        &ui::i18n::I18nManager::format(
            "canvas-layer-remove-name",
            &[("name", row.name.clone().into())],
        ),
        Artwork::REMOVE_LAYER_BY_ID.with(row.id.clone()),
    );
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_gap(0.0)
        .push(Box::new(
            Row::new()
                .with_gap(ui::tokens::Spacing::BASE.s1)
                .no_wrap()
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(Box::new(label))
                .push(Box::new(Spacer::new()))
                .push(masking)
                .push(Box::new(raise))
                .push(Box::new(lower))
                .push(Box::new(remove)),
        ))
        .push(Box::new(slider))
        .push(layer_row(cx, tools, &row));
    if let Some(mask) = &row.mask {
        column = column.push(mask_line(cx, &row, mask));
    }
    column = column.push(folder_field(cx, &row.id));
    Box::new(column)
}

/// The organisational folder the layer `id` is filed in, renamed in place.
fn folder_field(cx: &ViewCx<Artwork>, id: &str) -> Element {
    let folder = {
        let id = id.to_owned();
        cx.value(move |art: &Artwork| {
            art.layer(&id)
                .map(|layer| layer.group.clone())
                .unwrap_or_default()
        })
    };
    let id = id.to_owned();
    cx.switch(&folder, move |cx, folder| {
        Box::new(
            cx.actions().bind(
                EditableLabel::new(folder)
                    .edits_on_click()
                    .with_id("canvas-layer-folder")
                    .with_placeholder(ui::i18n::I18nManager::lookup_plain("canvas-layer-folder"))
                    .with_accessible_name(ui::i18n::I18nManager::lookup_plain(
                        "canvas-layer-folder",
                    ))
                    .filling(),
                Artwork::SET_LAYER_GROUP.map({
                    let id = id.clone();
                    move |group: String| (id.clone(), group)
                }),
            ),
        )
    })
}

/// Address objects independently of hit testing, including occluded members.
pub fn objects_panel(cx: &ViewCx<Artwork>, tools: &Tools) -> Element {
    let held = tools.clone();
    cx.switch(&tools.layer, move |cx, layer| objects(cx, layer, &held))
}

fn objects(cx: &ViewCx<Artwork>, layer: &str, tools: &Tools) -> Element {
    use ui::widget::{TreeNode, TreeView};
    let layer = layer.to_owned();
    let marks = cx.value(move |art: &Artwork| {
        art.marks_of(&layer)
            .map(|mark| (mark.id.clone(), mark.name.clone(), mark.group.clone()))
            .collect::<Vec<_>>()
    });
    let tools = tools.clone();
    cx.switch(&marks, move |_cx, marks| {
        let mut children: Vec<TreeNode<()>> = Vec::new();
        let mut targets: std::collections::HashMap<u64, Vec<String>> =
            std::collections::HashMap::new();
        let mut folders: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        let mut next = u64::try_from(marks.len()).unwrap_or(0).saturating_add(1);
        for (index, (id, name, group)) in marks.iter().rev().enumerate() {
            let node = u64::try_from(index).unwrap_or(0).saturating_add(1);
            let label = if name.is_empty() {
                id.clone()
            } else {
                name.clone()
            };
            let leaf = TreeNode::leaf(node, label, ());
            targets.insert(node, vec![id.clone()]);
            if group.is_empty() {
                children.push(leaf);
            } else {
                let folder = if let Some(index) = folders.get(group) {
                    *index
                } else {
                    let at = children.len();
                    folders.insert(group.clone(), at);
                    children.push(TreeNode::branch(next, group, (), Vec::new()));
                    next = next.saturating_add(1);
                    at
                };
                if let Some(branch) = children.get_mut(folder) {
                    targets.entry(branch.id).or_default().push(id.clone());
                    branch.children.push(leaf);
                }
            }
        }
        let picked = tools.pick_objects.clone();
        Box::new(
            TreeView::new(TreeNode::branch(0, "", (), children))
                .hide_root()
                .with_id("canvas-objects")
                .with_viewport(232.0, 28.0)
                .with_heading(ui::i18n::I18nManager::lookup_plain("canvas-objects"))
                .on_select(move |node| {
                    if let Some(ids) = targets.get(&node) {
                        picked.set(Some(ids.clone()));
                    }
                }),
        )
    })
}

/// Giving the chosen layer a mask, and clipping it to the layer below.
fn mask_buttons(cx: &ViewCx<Artwork>, row: &LayerRow) -> Element {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let mut buttons = Row::new()
        .with_gap(ui::tokens::Spacing::BASE.s1)
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center);
    if row.mask.is_none() {
        buttons = buttons.push(Box::new(icon_button(
            cx,
            Icon::SquareDashed,
            &lookup("canvas-layer-mask-add"),
            Artwork::SET_LAYER_MASK.with((row.id.clone(), "reveal".into())),
        )));
    }
    let mut clip = icon_button(
        cx,
        Icon::Paperclip,
        &lookup("canvas-command-layer-clip"),
        Artwork::SET_LAYER_CLIP.with((row.id.clone(), !row.clip)),
    );
    if row.clip {
        clip = clip.toggled(true);
    }
    Box::new(buttons.push(Box::new(clip)))
}

/// The chosen layer's mask: switch it off, invert it, remove it. It is
/// painted by choosing its picture in the layer's row.
fn mask_line(cx: &ViewCx<Artwork>, row: &LayerRow, mask: &MaskRow) -> Element {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let (eye, name) = if mask.visible {
        (Icon::Eye, "canvas-layer-mask-off")
    } else {
        (Icon::EyeOff, "canvas-layer-mask-on")
    };
    let mut invert = icon_button(
        cx,
        Icon::ArrowUpDown,
        &lookup("canvas-layer-mask-invert"),
        Artwork::SET_MASK_INVERT.with((row.id.clone(), !mask.invert)),
    );
    if mask.invert {
        invert = invert.toggled(true);
    }
    let (chain, link) = if mask.unlinked {
        (Icon::Unlink, "link")
    } else {
        (Icon::Link, "unlink")
    };
    let link = icon_button(
        cx,
        chain,
        &lookup(&format!("canvas-layer-mask-{link}")),
        Artwork::SET_LAYER_MASK.with((row.id.clone(), link.into())),
    );
    Box::new(
        Row::new()
            .with_gap(ui::tokens::Spacing::BASE.s1)
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(caption(&lookup("canvas-medium-mask")))
            .push(Box::new(Spacer::new()))
            .push(Box::new(icon_button(
                cx,
                eye,
                &lookup(name),
                Artwork::SET_LAYER_VISIBLE.with((mask.id.clone(), !mask.visible)),
            )))
            .push(Box::new(invert))
            .push(Box::new(link))
            .push(Box::new(icon_button(
                cx,
                Icon::X,
                &lookup("canvas-layer-mask-remove"),
                Artwork::SET_LAYER_MASK.with((row.id.clone(), "remove".into())),
            ))),
    )
}

/// Adding a layer: a menu of the media to paint it in, adding the chosen
/// one above the chosen layer and naming it for its medium.
pub fn add_menu(cx: &ViewCx<Artwork>, tools: &Tools) -> Element {
    let (media, names) = cx.read(|art| {
        let mut media = vec![(
            String::new(),
            ui::i18n::I18nManager::lookup_plain("canvas-medium-plain"),
        )];
        if let Some(art) = art {
            media.extend(
                crate::media::available(art)
                    .into_iter()
                    .map(|medium| (medium.id, medium.label)),
            );
        }
        let names: Vec<String> = art
            .map(|art| art.layers.iter().map(|layer| layer.name.clone()).collect())
            .unwrap_or_default();
        (media, names)
    });
    let state = LocalState::new(String::new());
    let mut menu = Dropdown::from_state(Vec::new(), &state)
        .as_menu(Icon::Plus)
        .with_id("canvas-add-layer")
        .with_accessible_name(ui::i18n::I18nManager::lookup_plain(
            "canvas-command-add-layer",
        ));
    menu.set_value_options(media.clone());
    let chosen = tools.layer.clone();
    let add = Artwork::ADD_LAYER.map(move |medium: String| {
        // A plain layer is "Layer 3"; "Oil, alla prima" names its layer
        // "Oil", and the next one "Oil 2".
        let label = media
            .iter()
            .find(|(id, _)| *id == medium)
            .map(|(_, label)| label.as_str())
            .unwrap_or_default();
        let base = if medium.is_empty() {
            "Layer"
        } else {
            label.split(',').next().unwrap_or_default().trim()
        };
        let name = crate::actions::numbered_name(names.iter().map(String::as_str), base);
        // Named here and chosen now, so the new layer is the one drawn on
        // next even before the artwork has it; it goes above the chosen one.
        let random = uuid::Uuid::new_v4().simple().to_string();
        let id = format!("layer-{}", random.get(..8).unwrap_or_default());
        let above = Some(chosen.with(Clone::clone)).filter(|above| !above.is_empty());
        chosen.set(id.clone());
        crate::actions::AddLayer {
            id: Some(id),
            name,
            medium: Some(medium),
            above,
            ..crate::actions::AddLayer::default()
        }
    });
    Box::new(cx.actions().bind(menu, add))
}
