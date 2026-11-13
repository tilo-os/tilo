use super::*;

fn shuffled_four() -> (Music, AutoCommit) {
    let (mut app, doc) = app_with_doc();
    app.disconnect();
    app.pb.queue = ["tk_g1", "tk_g2", "tk_g3", "tk_g4"]
        .map(String::from)
        .to_vec();
    app.pb.current = Some("tk_g1".into());
    app.pb.queue_index = 0;
    app.pb.shuffle = true;
    app.pb.playing = false;
    app.shuffle_order = vec![0, 3, 1, 2];
    (app, doc)
}

fn rest_of_shuffle(app: &mut Music) -> Vec<String> {
    let mut heard = Vec::new();
    app.pb.playing = true;
    for _ in 0..10 {
        app.advance(false);
        if !app.pb.playing {
            break;
        }
        heard.push(app.pb.current.clone().unwrap_or_default());
    }
    heard
}

#[test]
fn queue_edits_under_shuffle_keep_what_played_and_play_next_plays_next() {
    {
        let (mut app, mut doc) = shuffled_four();
        app.advance(false);
        assert_eq!(app.pb.current.as_deref(), Some("tk_g4"));
        app.dispatch(
            Action::PlayNext("tk_p1".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(rest_of_shuffle(&mut app), ["tk_p1", "tk_g2", "tk_g3"]);
    }
    {
        let (mut app, mut doc) = shuffled_four();
        app.shuffle_order.clear();
        app.dispatch(
            Action::PlayNext("tk_p1".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        let heard = rest_of_shuffle(&mut app);
        assert_eq!(heard.first().map(String::as_str), Some("tk_p1"));
        assert_eq!(heard.len(), 4, "every other song once: {heard:?}");
    }
    {
        let (mut app, mut doc) = shuffled_four();
        app.advance(false);
        app.dispatch(
            Action::AddToQueue("tk_p1".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(rest_of_shuffle(&mut app), ["tk_g2", "tk_g3", "tk_p1"]);
    }
    {
        let (mut app, mut doc) = shuffled_four();
        app.advance(false);
        app.dispatch(
            Action::MoveQueueItem { from: 3, to: 0 },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(app.pb.current.as_deref(), Some("tk_g4"));
        assert_eq!(rest_of_shuffle(&mut app), ["tk_g2", "tk_g3"]);
    }
    {
        let (mut app, mut doc) = shuffled_four();
        app.advance(false);
        app.dispatch(
            Action::RemoveFromQueue(1),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(rest_of_shuffle(&mut app), ["tk_g3"]);
    }
    {
        let (mut app, mut doc) = shuffled_four();
        app.dispatch(
            Action::RemoveFromQueue(0),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(app.pb.current.as_deref(), Some("tk_g4"));
        assert_eq!(rest_of_shuffle(&mut app), ["tk_g2", "tk_g3"]);
    }
    {
        use crate::actions::MusicEffect;
        let (mut app, _) = shuffled_four();
        app.advance(false);
        app.pb.playing = false;
        app.begin_effect(
            tilo_actions::InvocationId::new(),
            MusicEffect::PlayNext {
                track: "tk_p1".into(),
            },
        )
        .unwrap();
        app.begin_effect(
            tilo_actions::InvocationId::new(),
            MusicEffect::AddToQueue {
                track: "tk_p2".into(),
            },
        )
        .unwrap();
        assert_eq!(
            rest_of_shuffle(&mut app),
            ["tk_p1", "tk_g2", "tk_g3", "tk_p2"]
        );
    }
}

#[test]
fn an_agents_queue_edit_moves_the_copy_it_names_under_shuffle() {
    use crate::actions::MusicEffect;
    {
        let (mut app, _) = shuffled_four();
        app.shuffle_order = vec![1, 0, 2, 3];
        app.begin_effect(
            tilo_actions::InvocationId::new(),
            MusicEffect::PlayNext {
                track: "tk_g2".into(),
            },
        )
        .unwrap();
        assert_eq!(rest_of_shuffle(&mut app), ["tk_g2", "tk_g3", "tk_g4"]);
    }
    {
        let (mut app, _) = shuffled_four();
        app.pb.queue = ["tk_g1", "tk_g2", "tk_g2", "tk_g3"]
            .map(String::from)
            .to_vec();
        app.pb.queue_index = 1;
        app.pb.current = Some("tk_g2".into());
        app.shuffle_order = vec![0, 1, 3, 2];
        app.begin_effect(
            tilo_actions::InvocationId::new(),
            MusicEffect::QueueRemove {
                track: "music.track:tk_g2".into(),
            },
        )
        .unwrap();
        assert_eq!(app.pb.current.as_deref(), Some("tk_g3"));
        assert_eq!(rest_of_shuffle(&mut app), ["tk_g2"]);
    }
}
