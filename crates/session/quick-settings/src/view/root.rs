//! The controls page: the tiles two to a row, the sliders, what plays and
//! the rows under them; a tile's chevron or a row's disclosure opens that
//! section as its own page.

use ui::LocalState;
use ui::TextRole;
use ui::framework::ViewCx;
use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::primitives::Facts;
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

/// The controls page.
pub(super) fn page(cx: &ViewCx<QuickSettings>, parts: &Parts) -> Element {
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Section);
    // What the machine is connected to stands first: the tiles say at a
    // glance what is on, and the rest of the page reads under them.
    column = column.push(cx.switch(&parts.tiles, |cx, tiles: &Vec<TileFace>| {
        tile_grid(cx, tiles)
    }));
    column = column.push(cx.switch(&parts.sliders, |cx, sliders: &Sliders| {
        slider_block(cx, sliders)
    }));
    column = column.push(super::now_playing::card(cx, &parts.now_playing));
    column = column.push(
        cx.switch(&parts.rows, |cx, rows: &Vec<crate::faces::RowFace>| {
            let mut list = Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_rhythm(Rhythm::Tight);
            for row in rows {
                list = list.push(root_row(cx, row));
            }
            Box::new(list)
        }),
    );
    column.into_widget()
}

/// The sliders, a compact row each: what it sets, the track it moves, the
/// value it stands at, and the device under the track it sets it on.
fn slider_block(cx: &ViewCx<QuickSettings>, sliders: &Sliders) -> Element {
    let mut block = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Related);
    for face in &sliders.rows {
        block = block.push(slider_row(cx, face, sliders));
    }
    block.into_widget()
}

/// One slider's row: its icon (the volume's mutes it), the track over the
/// device it plays on, the value it stands at and, for sound, the chevron
/// that opens where it plays.
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
    let value = Text::new(&face.value)
        .with_role(TextRole::Label)
        .with_id(format!("{}-value", face.id));
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
                    .push(Box::new(value))
                    .push(trailing),
            )),
    )
}

fn slider(cx: &ViewCx<QuickSettings>, face: &SliderFace) -> Element {
    let level = LocalState::new(face.level);
    let slider = Slider::from_state(&level)
        .with_id(&face.id)
        .with_accessible_name(&face.name)
        .range(0.0, 1.0)
        .step(0.05)
        .commits_on_release();
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
fn tile_grid(cx: &ViewCx<QuickSettings>, tiles: &[TileFace]) -> Element {
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
fn root_row(cx: &ViewCx<QuickSettings>, face: &crate::faces::RowFace) -> Element {
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
