//! The inspector's part for a picture: cropping it on the canvas, fitting
//! the artwork to it, flipping and straightening it, and its colour and
//! tone adjustments. Each slider shows its value on the canvas while it is
//! dragged and becomes one edit, one step of history, when let go.

use std::rc::Rc;

use ui::LocalState;
use ui::prelude::*;
use ui::widget::Toggle;

use super::heading;
use super::inspector::{grow, labelled, section, tight};
use crate::model::{Adjust, Mark, Shape};
use crate::resource::ArtworkResource;
use crate::tools::{Live, Tools};

/// One adjustment a slider sets: its name, range and how it reads.
struct Step {
    /// The Fluent id of its name, and its control's id.
    id: &'static str,
    low: f64,
    high: f64,
    read: fn(&Adjust) -> Option<f64>,
    write: fn(&mut Adjust, f64),
    /// Decimals the value is kept to.
    places: i32,
}

static LIGHT: [Step; 4] = [
    Step {
        id: "canvas-adjust-exposure",
        low: -3.0,
        high: 3.0,
        read: |a| a.exposure,
        write: |a, v| a.exposure = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-contrast",
        low: -1.0,
        high: 1.0,
        read: |a| a.contrast,
        write: |a, v| a.contrast = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-highlights",
        low: -1.0,
        high: 1.0,
        read: |a| a.highlights,
        write: |a, v| a.highlights = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-shadows",
        low: -1.0,
        high: 1.0,
        read: |a| a.shadows,
        write: |a, v| a.shadows = Some(v),
        places: 2,
    },
];

static COLOUR: [Step; 5] = [
    Step {
        id: "canvas-adjust-temperature",
        low: -1.0,
        high: 1.0,
        read: |a| a.temperature,
        write: |a, v| a.temperature = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-tint",
        low: -1.0,
        high: 1.0,
        read: |a| a.tint,
        write: |a, v| a.tint = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-saturation",
        low: -1.0,
        high: 1.0,
        read: |a| a.saturation,
        write: |a, v| a.saturation = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-vibrance",
        low: -1.0,
        high: 1.0,
        read: |a| a.vibrance,
        write: |a, v| a.vibrance = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-hue",
        low: -180.0,
        high: 180.0,
        read: |a| a.hue,
        write: |a, v| a.hue = Some(v),
        places: 0,
    },
];

static EFFECTS: [Step; 3] = [
    Step {
        id: "canvas-adjust-blur",
        low: 0.0,
        high: 40.0,
        read: |a| a.blur,
        write: |a, v| a.blur = Some(v),
        places: 1,
    },
    Step {
        id: "canvas-adjust-sharpen",
        low: 0.0,
        high: 1.0,
        read: |a| a.sharpen,
        write: |a, v| a.sharpen = Some(v),
        places: 2,
    },
    Step {
        id: "canvas-adjust-vignette",
        low: -1.0,
        high: 1.0,
        read: |a| a.vignette,
        write: |a, v| a.vignette = Some(v),
        places: 2,
    },
];

impl Step {
    /// The value at `travel` (0-1) along the slider.
    fn at(&self, travel: f64) -> f64 {
        let value = self.low + travel.clamp(0.0, 1.0) * (self.high - self.low);
        let scale = 10_f64.powi(self.places);
        (value * scale).round() / scale
    }

    /// Where `value` sits along the slider.
    fn travel(&self, value: f64) -> f64 {
        ((value - self.low) / (self.high - self.low)).clamp(0.0, 1.0)
    }

    fn label(&self, value: f64) -> String {
        let places = usize::try_from(self.places).unwrap_or(0);
        if self.low < 0.0 && value > 0.0 {
            format!("+{value:.places$}")
        } else {
            format!("{value:.places$}")
        }
    }
}

/// The picture's part of the inspector: `mark` is the first of `ids`.
pub fn panel(cx: &ViewCx<ArtworkResource>, ids: &[String], mark: &Mark, tools: &Tools) -> Element {
    tools.live.borrow_mut().clear();
    let adjust = mark.style.adjust.clone().unwrap_or_default();
    let mut parts = vec![
        heading("canvas-picture"),
        framing(cx, ids, mark, tools),
        straighten(cx, ids, mark, tools),
    ];
    let reset = cx
        .button(
            "canvas-adjust-reset",
            ArtworkResource::SET_ADJUST.with((
                ids.to_vec(),
                Adjust {
                    reset: Some(true),
                    ..Adjust::default()
                },
            )),
        )
        .ghost();
    let title = Row::new()
        .with_gap(8.0)
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(heading("canvas-adjust"))
        .push(Box::new(Spacer::new()))
        .push(Box::new(reset));
    let mut adjust_parts: Vec<Element> = vec![Box::new(title)];
    for (group, steps) in [
        ("canvas-adjust-light", &LIGHT[..]),
        ("canvas-adjust-colour", &COLOUR[..]),
        ("canvas-adjust-effects", &EFFECTS[..]),
    ] {
        adjust_parts.push(Box::new(
            Text::new(group).with_role(TextRole::Caption).dimmed(),
        ));
        for step in steps {
            adjust_parts.push(slider(cx, ids, &adjust, step, tools));
        }
        if group == "canvas-adjust-colour" {
            adjust_parts.push(flag(
                cx,
                ids,
                "canvas-adjust-grayscale",
                adjust.grayscale,
                |a, on| {
                    a.grayscale = Some(on);
                },
            ));
            adjust_parts.push(flag(
                cx,
                ids,
                "canvas-adjust-invert",
                adjust.invert,
                |a, on| {
                    a.invert = Some(on);
                },
            ));
        }
    }
    parts.push(section(adjust_parts));
    section(parts)
}

/// Crop on the canvas, fit the artwork to the picture, flip it.
fn framing(cx: &ViewCx<ArtworkResource>, ids: &[String], mark: &Mark, tools: &Tools) -> Element {
    let crop = {
        let crop = tools.crop.clone();
        cx.switch(&tools.crop, move |_, cropping| {
            let id = if *cropping {
                "canvas-crop-done"
            } else {
                "canvas-crop"
            };
            let name = ui::i18n::I18nManager::lookup_plain(id);
            let flip = crop.clone();
            let button = Button::new(id, &name, None)
                .tooltip(ui::i18n::I18nManager::lookup_plain("canvas-crop-hint"))
                .on_view(move || flip.set(!flip.with(|on| *on)));
            Box::new(if *cropping { button.primary() } else { button }) as Element
        })
    };
    let fit = cx
        .button(
            "canvas-fit-artwork",
            ArtworkResource::FIT_ARTWORK.with(mark.id.clone()),
        )
        .tooltip(ui::i18n::I18nManager::lookup_plain(
            "canvas-fit-artwork-hint",
        ));
    let flip = |id: &str, way: &str| {
        Box::new(
            cx.button(
                id,
                ArtworkResource::FLIP.with((ids.to_vec(), way.to_owned())),
            )
            .ghost(),
        ) as Element
    };
    let row = |children: Vec<Element>| {
        let mut row = Row::new()
            .with_gap(6.0)
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center);
        for child in children {
            row = row.push(grow(child));
        }
        Box::new(row) as Element
    };
    section(vec![
        row(vec![crop, Box::new(fit)]),
        row(vec![
            flip("canvas-flip-horizontal", "horizontal"),
            flip("canvas-flip-vertical", "vertical"),
        ]),
    ])
}

/// The picture's turn inside its frame, -45° to 45°.
fn straighten(cx: &ViewCx<ArtworkResource>, ids: &[String], mark: &Mark, tools: &Tools) -> Element {
    const ID: &str = "canvas-straighten";
    let angle = match &mark.shape {
        Shape::Image { straighten, .. } => straighten.unwrap_or(0.0),
        _ => 0.0,
    };
    let to_value = |travel: f64| {
        let value = -45.0 + travel.clamp(0.0, 1.0) * 90.0;
        (value * 10.0).round() / 10.0
    };
    let travel = ((angle + 45.0) / 90.0).clamp(0.0, 1.0);
    let state = LocalState::new(travel);
    tools.live.borrow_mut().push(Live {
        ids: ids.to_vec(),
        state: state.clone(),
        committed: travel,
        apply: Rc::new(move |mark: &mut Mark, travel| {
            if let Some(mut placement) = crate::picture::Placement::of(&mark.shape) {
                placement.straighten = to_value(travel);
                placement.apply(&mut mark.shape);
            }
        }),
    });
    let shown = {
        let state = state.clone();
        Text::dynamic(move || format!("{:.1}°", to_value(state.with(|t| *t))))
            .with_role(TextRole::Caption)
    };
    let slider = cx.actions().bind(
        Slider::from_state(&state)
            .with_id(ID)
            .with_accessible_name(&ui::i18n::I18nManager::lookup_plain(ID))
            .commits_on_release(),
        ArtworkResource::SET_STRAIGHTEN.map({
            let ids = ids.to_vec();
            move |travel: f64| (ids.clone(), to_value(travel))
        }),
    );
    tight(vec![
        labelled(&ui::i18n::I18nManager::lookup_plain(ID), Box::new(shown)),
        Box::new(slider),
    ])
}

/// A slider for one adjustment of the marks `ids`.
fn slider(
    cx: &ViewCx<ArtworkResource>,
    ids: &[String],
    adjust: &Adjust,
    step: &'static Step,
    tools: &Tools,
) -> Element {
    let value = (step.read)(adjust).unwrap_or(0.0);
    let travel = step.travel(value);
    let state = LocalState::new(travel);
    tools.live.borrow_mut().push(Live {
        ids: ids.to_vec(),
        state: state.clone(),
        committed: travel,
        apply: Rc::new(move |mark: &mut Mark, travel| {
            let mut adjust = mark.style.adjust.clone().unwrap_or_default();
            (step.write)(&mut adjust, step.at(travel));
            mark.style.adjust = adjust.normalized();
        }),
    });
    let shown = {
        let state = state.clone();
        Text::dynamic(move || step.label(step.at(state.with(|t| *t)))).with_role(TextRole::Caption)
    };
    let control = cx.actions().bind(
        Slider::from_state(&state)
            .with_id(step.id)
            .with_accessible_name(&ui::i18n::I18nManager::lookup_plain(step.id))
            .commits_on_release(),
        ArtworkResource::SET_ADJUST.map({
            let ids = ids.to_vec();
            move |travel: f64| {
                let mut edit = Adjust::default();
                (step.write)(&mut edit, step.at(travel));
                (ids.clone(), edit)
            }
        }),
    );
    tight(vec![
        labelled(
            &ui::i18n::I18nManager::lookup_plain(step.id),
            Box::new(shown),
        ),
        Box::new(control),
    ])
}

/// A switch for an adjustment that is on or off.
fn flag(
    cx: &ViewCx<ArtworkResource>,
    ids: &[String],
    id: &str,
    on: Option<bool>,
    write: fn(&mut Adjust, bool),
) -> Element {
    let state = LocalState::new(on == Some(true));
    let toggle = cx.actions().bind(
        Toggle::from_state(&state)
            .with_id(id)
            .with_accessible_name(&ui::i18n::I18nManager::lookup_plain(id)),
        ArtworkResource::SET_ADJUST.map({
            let ids = ids.to_vec();
            move |on: bool| {
                let mut edit = Adjust::default();
                write(&mut edit, on);
                (ids.clone(), edit)
            }
        }),
    );
    labelled(&ui::i18n::I18nManager::lookup_plain(id), Box::new(toggle))
}
