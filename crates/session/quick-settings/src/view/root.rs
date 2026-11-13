//! The controls page: the tiles two to a row, the sliders, what plays and
//! the rows under them; a tile's chevron or a row's disclosure opens that
//! section in place, under it, and what follows moves down to make room.

use ui::framework::ViewCx;
use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::primitives::Facts;
use ui::widget::{
    Button, ButtonRole, Column, Container, DisclosureBand, Element, IconWidget, ListRow, QuickTile,
    Row, Spacer,
};

use super::Parts;
use crate::QsSection;
use crate::effect::QsEffect;
use crate::faces::{RootRow, RowFace, SliderFace, SliderKind, Sliders, TileFace};
use crate::host::Showing;
use crate::i18n::t;
use crate::resource::QuickSettings;

/// The controls page, built again with the section open on it.
pub(super) fn page(cx: &ViewCx<QuickSettings>, parts: &Parts) -> Element {
    let parts = parts.clone();
    cx.switch(&parts.open.clone(), move |cx, &open| {
        parts.host.showing(&Showing {
            section: open,
            ..Showing::default()
        });
        controls(cx, &parts, open)
    })
}

/// The controls with `open` unrolled under the control that opened it: under its tile's row,
/// the volume or its row. A section whose control is not on the page (one an action opened)
/// unrolls at the page's foot.
fn controls(cx: &ViewCx<QuickSettings>, parts: &Parts, open: Option<QsSection>) -> Element {
    let opener = |section: QsSection| {
        parts
            .tiles
            .with(|tiles| tiles.iter().any(|face| face.section == Some(section)))
            || parts
                .rows
                .with(|rows| rows.iter().any(|face| face.section() == Some(section)))
            || (section == QsSection::Sound && parts.sliders.with(|sliders| sliders.outputs))
    };
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Section);
    // What the machine is connected to stands first: the tiles say at a
    // glance what is on, and the rest of the page reads under them.
    let within = parts.clone();
    column = column.push(cx.switch(&parts.tiles, move |cx, tiles: &Vec<TileFace>| {
        tile_grid(cx, tiles, &within, open)
    }));
    let within = parts.clone();
    column = column.push(cx.switch(&parts.sliders, move |cx, sliders: &Sliders| {
        slider_block(cx, sliders, &within, open)
    }));
    column = column.push(super::now_playing::card(cx, &parts.now_playing));
    let within = parts.clone();
    column = column.push(cx.switch(&parts.rows, move |cx, rows: &Vec<RowFace>| {
        let mut list = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_rhythm(Rhythm::Tight);
        for row in rows {
            list = list.push(root_row(cx, row));
            if row.section().is_some() && row.section() == open {
                list = list.push(opened(cx, &within, row.section()));
            }
        }
        Box::new(list)
    }));
    if open.is_some_and(|section| !opener(section)) {
        column = column.push(opened(cx, parts, open));
    }
    column.into_widget()
}

/// `section`'s own rows, unrolled in place on the disclosure motion; nothing when none is open.
fn opened(cx: &ViewCx<QuickSettings>, parts: &Parts, section: Option<QsSection>) -> Element {
    let Some(section) = section else {
        return Box::new(Spacer::fixed(0.0));
    };
    Box::new(DisclosureBand::new(
        super::sections::section_id(section),
        super::sections::section(cx, section, parts),
    ))
}

/// The sliders, a compact row each: what it sets, the track it moves, the
/// value it stands at, and the device under the track it sets it on.
fn slider_block(
    cx: &ViewCx<QuickSettings>,
    sliders: &Sliders,
    parts: &Parts,
    open: Option<QsSection>,
) -> Element {
    let mut block = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Related);
    for face in &sliders.rows {
        block = block.push(slider_row(cx, face, sliders));
        if face.kind == SliderKind::Volume && sliders.outputs && open == Some(QsSection::Sound) {
            block = block.push(opened(cx, parts, open));
        }
    }
    block.into_widget()
}

/// One slider's row: its icon (the volume's mutes it), the track over the
/// device it plays on with the value it stands at beside it and, for sound,
/// the chevron that opens where it plays.
fn slider_row(cx: &ViewCx<QuickSettings>, face: &SliderFace, sliders: &Sliders) -> Element {
    let leading: Element = match face.kind {
        // With no device to set, the speaker says what the row would set and
        // takes no press.
        SliderKind::Volume if !face.available => Box::new(glyph(Icon::Volume2)),
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
            .toggled(sliders.muted);
            Box::new(cx.actions().bind_effect_invocation(
                mute,
                &QsEffect::SetMuted {
                    muted: !sliders.muted,
                },
            ))
        }
        _ => Box::new(glyph(Icon::Sun)),
    };
    let mut track = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Tight);
    // With no device to set, the words of the face stand where the track
    // would: no knob where nothing can move, and the row is one track tall,
    // as a row with a device is.
    track = track.push(if face.available {
        slider(cx, face)
    } else {
        Box::new(
            Container::new()
                .with_no_padding()
                .with_id(format!("{}-room", face.id))
                .with_child(Box::new(Facts::new(face.device.clone()))),
        )
    });
    if face.available && !face.device.is_empty() {
        track = track.push(Box::new(
            Container::new()
                .with_no_padding()
                .with_id(format!("{}-device", face.id))
                .with_child(Box::new(Facts::new(face.device.clone()))),
        ));
    }
    let trailing: Element = if face.kind == SliderKind::Volume && sliders.outputs {
        Box::new(
            cx.actions().bind_invocation(
                Button::icon_only(
                    "qs-sound-outputs",
                    Icon::ChevronRight,
                    &t("qs-sound-output"),
                )
                .role(ButtonRole::Plain),
                QuickSettings::TOGGLE_SECTION.with(QsSection::Sound),
            ),
        )
    } else if sliders.outputs {
        // Every value ends one glyph short of the page's edge, whether or
        // not its slider has a chevron.
        Box::new(glyph_space())
    } else {
        Box::new(Container::new().with_no_padding())
    };
    Box::new(
        Container::new()
            .with_id(format!("{}-row", face.id))
            .with_no_padding()
            .with_child(Box::new(
                Row::new()
                    .no_wrap()
                    .with_align_items(ui::taffy::AlignItems::Center)
                    .with_rhythm(Rhythm::Related)
                    .push(leading)
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_flex_grow()
                            .with_child(track.into_widget()),
                    ))
                    .push(trailing),
            )),
    )
}

fn slider(cx: &ViewCx<QuickSettings>, face: &SliderFace) -> Element {
    let said = face.clone();
    // The slider shows its value beside its track, clear of its knob, in the words it is
    // spoken in: the level, or that sound is muted.
    let slider = super::sections::level_slider(&face.id, &face.name, false, face.level)
        .value_text(move |level| said.value_at(level))
        .shows_value();
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

/// A glyph in the leading column of a slider.
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

/// The tiles two to a row.
fn tile_grid(
    cx: &ViewCx<QuickSettings>,
    tiles: &[TileFace],
    parts: &Parts,
    open: Option<QsSection>,
) -> Element {
    let mut grid = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Sibling);
    for pair in tiles.chunks(2) {
        // The row is as tall as the tile whose state line needs the second
        // line, and both tiles take that height: two to a row, one shape.
        let mut row = Row::new()
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Stretch)
            .with_rhythm(Rhythm::Sibling);
        for face in pair {
            row = row.push(tile(cx, face));
        }
        if pair.len() == 1 {
            // A lone tile keeps a tile's width.
            row = row.push(Box::new(
                Container::new().with_no_padding().with_flex_grow(),
            ));
        }
        grid = grid.push(Box::new(row));
        if pair
            .iter()
            .any(|face| face.section.is_some() && face.section == open)
        {
            grid = grid.push(opened(cx, parts, open));
        }
    }
    grid.into_widget()
}

fn tile(cx: &ViewCx<QuickSettings>, face: &TileFace) -> Element {
    let id = format!("qs-tile-{}", crate::faces::control_id(face.control));
    let mut tile = QuickTile::new(&id, face.icon, &face.label)
        .detail(&face.detail)
        .on(face.on)
        // Two to a row: the tile is as tall as the state line it will have
        // in half the page's width.
        .share(0.5)
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
        tile = tile.with_details(details_name(section));
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

/// A row under the tiles: it opens its section's page, or, for the
/// notifications, the desk's centre — the one list.
fn root_row(cx: &ViewCx<QuickSettings>, face: &RowFace) -> Element {
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
            row.disclosure(),
            QuickSettings::TOGGLE_SECTION.with(section),
        )
    } else {
        debug_assert_eq!(face.row, RootRow::Notifications);
        // The one list is the shell's centre, opened over the desk.
        actions.bind_effect_invocation(row.disclosure(), &QsEffect::ShowCentre)
    })
}

/// The panel's footer: who is signed in on the left, then the controls a
/// panel's bottom edge is reached for — arranging, Settings, Lock and the
/// power menu — grouped at the right. Each is a glyph with its name in its
/// tooltip; arranging is the one with its name beside it, because a pencil
/// alone does not say what it arranges.
pub(super) fn footer(cx: &ViewCx<QuickSettings>) -> Element {
    let actions = cx.actions();
    let account = actions.bind_effect_invocation(
        Button::icon_only("qs-account", Icon::User, &t("qs-account"))
            .role(ButtonRole::Plain)
            .tooltip(t("qs-account-description")),
        &QsEffect::OpenSettings {
            link: "settings://users".to_owned(),
        },
    );
    let edit = actions.bind_invocation(
        Button::new("qs-edit", &t("qs-edit-short"))
            .with_icon(Icon::Pencil)
            .role(ButtonRole::Plain)
            .tooltip(t("qs-edit-controls")),
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
        Container::new()
            .with_no_padding()
            .with_inset(Rhythm::Related)
            .with_child(Box::new(
                Row::new()
                    .no_wrap()
                    .with_align_items(ui::taffy::AlignItems::Center)
                    .with_rhythm(Rhythm::Related)
                    .push(Box::new(account))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(edit))
                    .push(Box::new(settings))
                    .push(Box::new(lock))
                    .push(Box::new(power)),
            )),
    )
}
