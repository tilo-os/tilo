//! A Rust app's `#[resource]` type is found by what it is, exactly as a
//! tilolang type is: search names its app by its registry words and its
//! things by their type's noun, with nothing the app writes for it.
//!
//! The types here are Tube's: a channel and a video of a video client whose
//! published title is a name of its own, found by the client's keyword
//! ("youtube"). The test is the one the design names: "youtube videos of
//! <channel>".

#![allow(clippy::expect_used, reason = "test assertions")]

use tilo_actions::ResourceSpec;
use tilo_resource::{Model, resource};

/// A channel of a video client, a `#[resource]` type: its words — its noun,
/// its app's name — come from its declaration, not from a line of search
/// code.
#[derive(Model, Default, Clone)]
pub struct Channel {
    /// Channel
    #[model(label)]
    pub name: String,
}

#[resource(kind = "tube.channel", catalogue = "tests/locales")]
impl Channel {}

/// A video of the same client.
#[derive(Model, Default, Clone)]
pub struct Video {
    /// Video
    #[model(label)]
    pub title: String,
}

#[resource(kind = "tube.video", catalogue = "tests/locales")]
impl Video {}

/// The open a catalogue row offers, so search has a row that opens one.
fn open() -> tilo_actions::ActionSpec {
    tilo_actions::ActionSpec::immediate_encoded(
        "open",
        "Open",
        tilo_actions::ExecutionLocus::ClientLocal,
        tilo_actions::EncodedAction {
            type_id: "test.action".into(),
            payload: Vec::new(),
        },
    )
}

fn published(resource: ResourceSpec) -> ResourceSpec {
    resource.with_action(open())
}

/// What a resource says it is, summarised as search reads it.
struct WhatItIs {
    noun: (String, String),
    app: (String, String),
    root: bool,
}

impl WhatItIs {
    fn of(resource: &ResourceSpec) -> Self {
        let kind = resource
            .resource_type
            .as_ref()
            .expect("every published resource says what it is");
        Self {
            noun: (kind.noun.one.clone(), kind.noun.many.clone()),
            app: (kind.app.one.clone(), kind.app.many.clone()),
            root: kind.root,
        }
    }
}

/// Whether a search for `needle` finds `resource` by what it is, as the one
/// search asks (`ui::search::system::type_match_score_with` over the nouns,
/// the app's name, the registry keywords and what the app's other types are
/// called, and its text).
fn found(
    resource: &ResourceSpec,
    needle: &str,
    keywords: &[&str],
    others: &[tilo_actions::Noun],
) -> bool {
    let ty = resource
        .resource_type
        .as_ref()
        .expect("every published resource says what it is");
    let needle = ui::search::system::without_filler(needle);
    let text = |rest: &str| {
        ui::search::system::rank_action_fields(
            rest,
            &[(resource.label.as_str(), ui::search::FieldWeight::TITLE)],
        )
    };
    ui::search::system::type_match_score_with(&needle, ty, others, keywords, text).is_some()
}

/// Tube's other types, as the one search passes them: "videos" names the
/// `Video` type, so it narrows a query to the client's videos rather than
/// naming the app.
fn siblings() -> [tilo_actions::Noun; 2] {
    [
        tilo_actions::Noun::new("channel", "channels"),
        tilo_actions::Noun::new("video", "videos"),
    ]
}

#[test]
fn a_rust_types_channel_says_what_it_is_from_its_declaration() {
    tilo_resource::words::name_app("Tube", "tube.library");
    let channel = published(tilo_resource::publish::<Channel>(
        "tube.channel:1",
        "MKBHD",
        &Channel {
            name: "MKBHD".to_owned(),
        },
    ));
    let what = WhatItIs::of(&channel);
    assert_eq!(what.noun, ("channel".to_owned(), "channels".to_owned()));
    assert_eq!(what.app, ("Tube".to_owned(), "tubes".to_owned()));
    assert!(!what.root);

    let video = WhatItIs::of(&published(tilo_resource::publish::<Video>(
        "tube.video:1",
        "Studio tour",
        &Video {
            title: "Studio tour".to_owned(),
        },
    )));
    assert_eq!(video.noun, ("video".to_owned(), "videos".to_owned()));
    assert_eq!(video.app, what.app);
    assert!(!video.root);
}

#[test]
fn a_rust_apps_channel_is_found_by_youtube_videos_of_its_name() {
    tilo_resource::words::name_app("Tube", "tube.library");
    // The registry's words for Tube (apps/tube/search/en.ftl): search names
    // the app by them as by its title.
    let keywords = ["youtube", "video", "videos", "channel", "channels"];
    let channel = published(tilo_resource::publish::<Channel>(
        "tube.channel:1",
        "MKBHD",
        &Channel {
            name: "MKBHD".to_owned(),
        },
    ));
    let naming = published(tilo_resource::publish::<Video>(
        "tube.video:1",
        "Studio tour with MKBHD",
        &Video {
            title: "Studio tour with MKBHD".to_owned(),
        },
    ));
    let other = published(tilo_resource::publish::<Video>(
        "tube.video:2",
        "Bread at home",
        &Video {
            title: "Bread at home".to_owned(),
        },
    ));

    let siblings = siblings();
    assert!(
        found(&channel, "youtube videos of mkbhd", &keywords, &siblings),
        "the channel is found by the client's word and its own name"
    );
    assert!(
        found(&naming, "youtube videos of mkbhd", &keywords, &siblings),
        "the video that names the channel is found too"
    );
    assert!(
        !found(&other, "youtube videos of mkbhd", &keywords, &siblings),
        "a video that does not name it is not"
    );
    assert!(
        !found(&channel, "youtube videos", &keywords, &siblings),
        "the videos, not the channel"
    );
    assert!(
        found(&other, "youtube videos", &keywords, &siblings),
        "every video of the client's is"
    );
}
