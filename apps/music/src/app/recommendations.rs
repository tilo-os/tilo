use ui::paint::PaintCtx;
use ui::tokens::TextRole;
use ui::widget::{Button, Column, Element, Row, Text, Toggle};

use super::{Action, MusicApp, View, page_part};
use crate::model::Library;
use crate::recommendations::Preferences;
use crate::recommendations::{Input, suggestions};

#[derive(Debug)]
struct Pick {
    track: String,
    title: String,
    artist: String,
    artist_name: String,
    reason: String,
    preferred: bool,
}

/// Retained callbacks capture presentation data, never the whole library.
#[derive(Debug)]
struct Presentation {
    picks: Vec<Pick>,
    inputs: Vec<(Input, bool)>,
    preferred: Vec<(String, String)>,
    hidden: Vec<(String, String)>,
    empty: bool,
    editable: bool,
}

impl Presentation {
    fn new(lib: &Library, details: bool, editable: bool) -> Self {
        let picks = suggestions(lib, if details { 6 } else { 3 })
            .into_iter()
            .filter_map(|s| {
                lib.track(&s.track).map(|t| Pick {
                    track: t.id.clone(),
                    title: t.title.clone(),
                    artist: t.artist_id.clone(),
                    artist_name: lib.artist_name(&t.artist_id).to_owned(),
                    reason: s.reason.label(lib),
                    preferred: lib.recommendations.preferred_artists.contains(&t.artist_id),
                })
            })
            .collect();
        let preferred = if details {
            lib.recommendations
                .preferred_artists
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        lib.artist(id)
                            .map_or(id.as_str(), |a| a.name.as_str())
                            .to_owned(),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let hidden = if details {
            lib.recommendations
                .dismissed
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        lib.track(id)
                            .map_or(id.as_str(), |t| t.title.as_str())
                            .to_owned(),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        Self {
            picks,
            inputs: Input::ALL
                .into_iter()
                .map(|i| (i, lib.recommendations.enabled(i)))
                .collect(),
            preferred,
            hidden,
            empty: lib.tracks.is_empty(),
            editable,
        }
    }
}

struct Controls<'a> {
    asked: &'a page_part::Asked,
    editable: bool,
}

fn button(asked: &Controls<'_>, id: &str, key: &str, action: Action) -> Element {
    let enabled = asked.editable || !matches!(action, Action::Recommendation(_));
    Box::new(
        Button::new(id, key)
            .with_enabled(enabled)
            .on_click(page_part::asking(asked.asked, action)),
    )
}

fn heading(key: &str) -> Element {
    Box::new(Text::new(key).with_role(TextRole::Heading))
}

fn inputs(mut content: Column, asked: &Controls<'_>, values: Vec<(Input, bool)>) -> Column {
    for (input, enabled) in values {
        let queue = asked.asked.clone();
        let id = format!("music-recommendations-input-{}", input.key());
        content = content.push(Box::new(
            Row::new().push(Box::new(Text::new(&id))).push(Box::new(
                Toggle::from_state(&ui::LocalState::new(enabled))
                    .with_id(&id)
                    .with_enabled(asked.editable)
                    .with_accessible_name(&input.label())
                    .on_change(move |enabled| {
                        queue.borrow_mut().push(Action::Recommendation(
                            Preferences::input_path(input).set(enabled),
                        ));
                    }),
            )),
        ));
    }
    content.push(Box::new(Text::new("music-recommendations-privacy")))
}

fn pick_controls(pick: &Pick, asked: &Controls<'_>, details: bool) -> Element {
    let queue = std::sync::Arc::new(vec![pick.track.clone()]);
    let mut controls = Row::new().push(button(
        asked,
        &format!("music-recommendations-play-{}", pick.track),
        "music-recommendations-play",
        Action::Play { queue, index: 0 },
    ));
    if details {
        controls = controls
            .push(button(
                asked,
                &format!("music-recommendations-dismiss-{}", pick.track),
                "music-recommendations-dismiss",
                Action::Recommendation(
                    Library::paths()
                        .recommendations
                        .dismissed
                        .insert(&pick.track),
                ),
            ))
            .push(button(
                asked,
                &format!("music-recommendations-prefer-{}", pick.track),
                if pick.preferred {
                    "music-recommendations-unprefer"
                } else {
                    "music-recommendations-prefer"
                },
                Action::Recommendation({
                    let preferred = Library::paths().recommendations.preferred_artists;
                    if pick.preferred {
                        preferred.remove(&pick.artist)
                    } else {
                        preferred.insert(&pick.artist)
                    }
                }),
            ));
    }
    Box::new(controls)
}

fn pick_card(pick: Pick, asked: &Controls<'_>, details: bool) -> Element {
    let controls = pick_controls(&pick, asked, details);
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(
                Text::with_args(
                    "music-recommendations-song",
                    &[
                        ("title", pick.title.into()),
                        ("artist", pick.artist_name.into()),
                    ],
                )
                .with_role(TextRole::Heading),
            ))
            .push(Box::new(Text::new(&pick.reason)))
            .push(controls),
    )
}

fn feedback(mut content: Column, asked: &Controls<'_>, presentation: Presentation) -> Column {
    content = content
        .push(heading("music-recommendations-feedback"))
        .push(Box::new(Text::new("music-recommendations-feedback-help")));
    if presentation.preferred.is_empty() && presentation.hidden.is_empty() {
        content = content.push(Box::new(Text::new("music-recommendations-feedback-empty")));
    }
    for (artist, name) in presentation.preferred {
        content = content
            .push(Box::new(Text::with_args(
                "music-recommendations-preferred-artist",
                &[("name", name.into())],
            )))
            .push(button(
                asked,
                &format!("music-recommendations-unprefer-{artist}"),
                "music-recommendations-unprefer",
                Action::Recommendation(
                    Library::paths()
                        .recommendations
                        .preferred_artists
                        .remove(&artist),
                ),
            ));
    }
    for (track, title) in presentation.hidden {
        content = content
            .push(Box::new(Text::with_args(
                "music-recommendations-hidden-song",
                &[("title", title.into())],
            )))
            .push(button(
                asked,
                &format!("music-recommendations-restore-{track}"),
                "music-recommendations-restore",
                Action::Recommendation(Library::paths().recommendations.dismissed.remove(&track)),
            ));
    }
    content
}

fn page(mut presentation: Presentation, details: bool, queue: &page_part::Asked) -> Element {
    let asked = &Controls {
        asked: queue,
        editable: presentation.editable,
    };
    let mut content = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(heading("music-recommendations-title"))
        .push(Box::new(Text::new("music-recommendations-help")));
    if details {
        content = inputs(content, asked, std::mem::take(&mut presentation.inputs));
    } else {
        content = content.push(button(
            asked,
            "music-recommendations-tune",
            "music-recommendations-tune",
            Action::Nav(View::Recommendations),
        ));
    }
    if presentation.picks.is_empty() {
        content = content
            .push(Box::new(Text::new(if presentation.empty {
                "music-recommendations-empty"
            } else {
                "music-recommendations-no-match"
            })))
            .push(button(
                asked,
                "music-recommendations-library",
                "music-recommendations-library",
                Action::Nav(View::Songs),
            ));
    }
    for pick in std::mem::take(&mut presentation.picks) {
        content = content.push(pick_card(pick, asked, details));
    }
    if details {
        content = feedback(content, asked, presentation);
    }
    Box::new(content)
}

impl MusicApp {
    pub(super) fn draw_recommendations(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        span: (f64, f64),
        y: f64,
        details: bool,
    ) -> f64 {
        let editable = self
            .dispatcher
            .as_ref()
            .is_some_and(|dispatcher| !dispatcher.read_only());
        let presentation = Presentation::new(&self.lib, details, editable);
        let key = format!("recommendations:{details}:{presentation:?}");
        self.page_part(ctx, span, y, key, move |asked| {
            page(presentation, details, asked)
        })
    }
}
