//! Starting a song's stream while another one is still starting: the
//! newest choice is the one that plays.

use super::*;

fn app_with_doc() -> (MusicApp, AutoCommit) {
    let mut app = MusicApp::new();
    let doc = crate::seed::sample_document();
    app.reload(&doc);
    (app, doc)
}

#[test]
fn next_pressed_again_while_a_stream_starts_supersedes_it() {
    use crate::actions::{MusicEffect, effect_payload};
    let (mut app, _doc) = app_with_doc();
    app.disconnect();
    let first = tilo_actions::InvocationId::new();
    app.start_effect(
        first,
        &effect_payload(&MusicEffect::Play {
            track: "tk_g1".into(),
            queued: None,
        }),
    )
    .unwrap();
    for track in ["tk_g2", "tk_g3"] {
        app.start_effect(
            tilo_actions::InvocationId::new(),
            &effect_payload(&MusicEffect::AddToQueue {
                track: track.into(),
            }),
        )
        .unwrap();
    }
    assert!(
        app.resource_stream_invocation.is_some(),
        "the first song's stream is still starting"
    );

    let second = tilo_actions::InvocationId::new();
    app.start_effect(second, &effect_payload(&MusicEffect::Next))
        .expect("Next is taken while a stream starts");
    assert_eq!(app.pb.current.as_deref(), Some("tk_g2"));
    let Some(tilo_actions::InvocationUpdate::Failed { error, .. }) = app.take_effect_result(first)
    else {
        panic!("the start Next replaced settles as superseded");
    };
    assert_eq!(error.code, "music.stream_start_superseded");

    app.start_effect(
        tilo_actions::InvocationId::new(),
        &effect_payload(&MusicEffect::Next),
    )
    .expect("and so is the Next after it");
    assert_eq!(app.pb.current.as_deref(), Some("tk_g3"));
    assert!(matches!(
        app.take_effect_result(second),
        Some(tilo_actions::InvocationUpdate::Failed { .. })
    ));
}
