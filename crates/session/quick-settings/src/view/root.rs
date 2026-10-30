//! The controls page: the sliders, the tiles two to a row, the rows under
//! them and the footer, each tile's or row's section opening and closing in
//! place under it on the disclosure motion.

use ui::LocalState;
use ui::TextRole;
use ui::framework::ViewCx;
use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, Column, Container, Element, IconWidget, ListRow, QuickTile, Row, Slider,
    Spacer, Text,
};

use super::Parts;
use crate::QsSection;
use crate::effect::QsEffect;
use crate::faces::{RootRow, SliderFace, SliderKind, Sliders, TileFace};
use crate::i18n::t;
use crate::resource::QuickSettings;

/// The controls page with `open` open in place.
pub(super) fn page(cx: &ViewCx<QuickSettings>, open: Option<QsSection>, parts: &Parts) -> Element {
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Section);
    let sound = open == Some(QsSection::Sound);
    let sliders = cx.switch(&parts.sliders, move |cx, sliders| {
        slider_block(cx, sliders, sound)
    });
    column = column.push(sliders);
    if sound {
        column = column.push(super::sections::in_place(cx, QsSection::Sound, parts));
    }
    let tile_parts = parts.clone();
    column = column.push(cx.switch(&parts.tiles, move |cx, tiles| {
        tile_grid(cx, tiles, open, &tile_parts)
    }));
    let row_parts = parts.clone();
    column = column.push(cx.switch(&parts.rows, move |cx, rows| {
        let mut list = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_rhythm(Rhythm::Tight);
        for row in rows {
            list = list.push(root_row(cx, row, open));
            if let Some(section) = row.section().filter(|section| open == Some(*section)) {
                list = list.push(super::sections::in_place(cx, section, &row_parts));
            }
        }
        Box::new(list)
    }));
    column.push(footer(cx)).into_widget()
}

/// The sliders: brightness (one per display, captioned with its name when
/// there are several) and volume, with the speaker that mutes and, when
/// there are outputs to choose from, the chevron that opens them.
fn slider_block(cx: &ViewCx<QuickSettings>, sliders: &Sliders, sound_open: bool) -> Element {
    let mut block = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Related);
    for face in &sliders.rows {
        if let Some(caption) = &face.caption {
            block = block.push(Box::new(Text::new(caption).with_role(TextRole::Caption)));
        }
        let leading: Element = match face.kind {
            SliderKind::Volume => {
                let mute = Button::icon_only(
                    "qs-volume-mute",
                    if sliders.muted {
                        Icon::VolumeX
                    } else {
                        Icon::Volume2
                    },
                    &t("qs-volume-mute"),
                )
                .role(ButtonRole::Plain)
                .toggled(sliders.muted)
                .with_enabled(face.available);
                Box::new(cx.actions().bind_effect_invocation(
                    mute,
                    &QsEffect::SetMuted {
                        muted: !sliders.muted,
                    },
                ))
            }
            _ => Box::new(glyph(Icon::Sun)),
        };
        let trailing: Element = if face.kind == SliderKind::Volume && sliders.outputs {
            Box::new(
                cx.actions().bind_invocation(
                    Button::icon_only(
                        "qs-sound-outputs",
                        if sound_open {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        },
                        &t("qs-sound-output"),
                    )
                    .role(ButtonRole::Plain)
                    .toggled(sound_open),
                    QuickSettings::TOGGLE_SECTION.with(QsSection::Sound),
                ),
            )
        } else if sliders.outputs {
            // Every track is one length, whether or not it has a chevron.
            Box::new(glyph_space())
        } else {
            Box::new(Spacer::new())
        };
        block = block.push(Box::new(
            Row::new()
                .no_wrap()
                .with_align_items(ui::taffy::AlignItems::Center)
                .with_rhythm(Rhythm::Related)
                .push(leading)
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .with_child(slider(cx, face)),
                ))
                .push(trailing),
        ));
    }
    block.into_widget()
}

fn slider(cx: &ViewCx<QuickSettings>, face: &SliderFace) -> Element {
    let level = LocalState::new(face.level);
    let slider = Slider::from_state(&level)
        .with_id(&face.id)
        .with_accessible_name(&face.name)
        .range(0.0, 1.0)
        .step(0.05)
        .commits_on_release()
        .with_enabled(face.available);
    let actions = cx.actions();
    Box::new(match &face.kind {
        SliderKind::Brightness => actions.bind_effect(slider, QsEffect::SET_BRIGHTNESS),
        SliderKind::Volume => actions.bind_effect(slider, QsEffect::SET_VOLUME),
        SliderKind::Display(display) => {
            let display = display.clone();
            actions.bind_effect(
                slider,
                QsEffect::SET_DISPLAY_BRIGHTNESS.map(move |level: f64| (display.clone(), level)),
            )
        }
    })
}

/// A glyph in the leading column of a slider or row.
fn glyph(icon: Icon) -> Container {
    Container::new()
        .with_no_padding()
        .with_size(GLYPH_COLUMN, GLYPH_COLUMN)
        .with_child(Box::new(IconWidget::new(icon)))
}

/// The room a slider's chevron takes, left empty.
fn glyph_space() -> Container {
    Container::new()
        .with_no_padding()
        .with_size(GLYPH_COLUMN, GLYPH_COLUMN)
}

/// The width of a slider's leading and trailing controls: a pointer target.
const GLYPH_COLUMN: f32 = ui::tokens::Metrics::BASE.min_pointer_target;

/// The tiles two to a row, the open tile's section under its row.
fn tile_grid(
    cx: &ViewCx<QuickSettings>,
    tiles: &[TileFace],
    open: Option<QsSection>,
    parts: &Parts,
) -> Element {
    let mut grid = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Sibling);
    for pair in tiles.chunks(2) {
        let mut row = Row::new().no_wrap().with_rhythm(Rhythm::Sibling);
        for face in pair {
            row = row.push(tile(cx, face, open));
        }
        if pair.len() == 1 {
            // A lone tile keeps a tile's width.
            row = row.push(Box::new(
                Container::new().with_no_padding().with_flex_grow(),
            ));
        }
        grid = grid.push(Box::new(row));
        if let Some(section) = pair
            .iter()
            .filter_map(|face| face.section)
            .find(|section| open == Some(*section))
        {
            grid = grid.push(super::sections::in_place(cx, section, parts));
        }
    }
    grid.into_widget()
}

fn tile(cx: &ViewCx<QuickSettings>, face: &TileFace, open: Option<QsSection>) -> Element {
    let id = format!("qs-tile-{}", crate::faces::control_id(face.control));
    let mut tile = QuickTile::new(&id, face.icon, &face.label)
        .detail(&face.detail)
        .on(face.on)
        .enabled(face.switch.is_some());
    let actions = cx.actions();
    if let Some(binding) = face
        .switch
        .as_ref()
        .and_then(|effect| actions.effect(effect))
    {
        tile = tile.on_switch(binding);
    }
    if let Some(section) = face.section {
        tile = tile
            .with_details(details_name(section))
            .details_open(if open == Some(section) { 1.0 } else { 0.0 });
        if let Some(binding) = actions.invocation(QuickSettings::TOGGLE_SECTION.with(section)) {
            tile = tile.on_details(binding);
        }
    }
    Box::new(tile)
}

/// The accessible name of a tile's chevron.
fn details_name(section: QsSection) -> String {
    t(match section {
        QsSection::Wifi => "qs-network-details",
        QsSection::Bluetooth => "qs-bluetooth-details",
        QsSection::Vpn => "qs-vpn-details",
        _ => "qs-power-mode",
    })
}

/// A row under the tiles: it opens its section in place, or (the
/// notifications) its page.
fn root_row(
    cx: &ViewCx<QuickSettings>,
    face: &crate::faces::RowFace,
    open: Option<QsSection>,
) -> Element {
    let mut row = ListRow::new(face.id(), &face.title).icon(face.icon);
    if let Some(detail) = &face.detail {
        row = row.secondary(detail);
    }
    if let Some(value) = &face.value {
        row = row.value(value);
    }
    let actions = cx.actions();
    Box::new(if let Some(section) = face.section() {
        actions.bind_invocation(
            row.disclosure().selected(open == Some(section)),
            QuickSettings::TOGGLE_SECTION.with(section),
        )
    } else {
        debug_assert_eq!(face.row, RootRow::Notifications);
        actions.bind_invocation(row.disclosure(), QuickSettings::SHOW_NOTIFICATIONS.invoke())
    })
}

/// Edit at the leading end; Settings, Lock and Power at the trailing one.
/// Each is named, and the name is its tooltip.
fn footer(cx: &ViewCx<QuickSettings>) -> Element {
    let actions = cx.actions();
    let edit = actions.bind_invocation(
        Button::icon_only("qs-edit", Icon::Pencil, &t("qs-edit-controls")).role(ButtonRole::Plain),
        QuickSettings::EDIT.with(true),
    );
    let settings = actions.bind_effect_invocation(
        Button::icon_only("qs-settings", Icon::Settings2, &t("qs-tile-settings"))
            .role(ButtonRole::Plain)
            .tooltip(t("qs-tile-settings-description")),
        &QsEffect::OpenSettings {
            link: String::new(),
        },
    );
    let lock = actions.bind_effect_invocation(
        Button::icon_only("qs-lock", Icon::Lock, &t("power-lock"))
            .role(ButtonRole::Plain)
            .tooltip(t("qs-lock-description")),
        &QsEffect::LockScreen,
    );
    let power = actions.bind_effect_invocation(
        Button::icon_only("qs-power", Icon::Power, &t("qs-tile-power"))
            .role(ButtonRole::Plain)
            .tooltip(t("qs-tile-power-description")),
        &QsEffect::PowerMenu,
    );
    Box::new(
        Row::new()
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .with_rhythm(Rhythm::Related)
            .push(Box::new(edit))
            .push(Box::new(Spacer::new()))
            .push(Box::new(settings))
            .push(Box::new(lock))
            .push(Box::new(power)),
    )
}
