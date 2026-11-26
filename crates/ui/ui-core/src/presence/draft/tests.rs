use crate::time::Duration;
use tilo_ui_base::collaboration::PresencePayload;
use tilo_ui_base::permissions::MemberId;
use tilo_ui_base::presence::COMPOSING_LINGER;
use tilo_ui_base::presence::PresenceKey;
use tilo_ui_base::presence::draft::*;
use tilo_ui_base::{presence::PresenceCtx, presence::capability};

fn channel() -> PresenceKey {
    PresenceKey::resource("so.tilo.chat", "channel-1", "TextChannel")
}

/// Ada's window says of her draft what `draft` holds at each moment, and Bo's window takes
/// what it says: whether Bo reads her typing, each 100 ms for 30 s, the clock driven by the
/// test, and how often her window said she was.
fn read_by_bo(
    mut draft: impl FnMut(Duration) -> Option<(PresenceKey, u64)>,
) -> (Vec<(Duration, bool)>, usize) {
    let start = crate::time::now();
    let ada = MemberId::person("ada");
    let mut bo = PresenceCtx::default();
    let mut said = DraftSaid::default();
    let mut read = Vec::new();
    let mut writing = 0;
    let mut at = Duration::ZERO;
    while at <= Duration::from_secs(30) {
        let now = start + at;
        if let Some((key, payload)) = said.look(draft(at), now) {
            writing += usize::from(payload == PresencePayload::Composing { composing: true });
            bo.apply_remote_presence_at(ada.clone(), key, payload, now);
        }
        bo.prune_composing(now);
        read.push((at, bo.composing_at(&channel(), now).contains(&ada)));
        at += Duration::from_millis(100);
    }
    (read, writing)
}

/// Ada types a key each 250 ms for `typed`, then sends her draft or leaves it.
fn typing(typed: Duration, sent: bool) -> impl FnMut(Duration) -> Option<(PresenceKey, u64)> {
    let mut revision = 0;
    let mut next_key = Duration::ZERO;
    move |at| {
        if at < typed && at >= next_key {
            revision += 1;
            next_key += Duration::from_millis(250);
        }
        (at < typed || !sent).then(|| (channel(), revision))
    }
}

#[test]
fn someone_typing_reads_as_typing_until_they_send_or_stop() {
    let typed = Duration::from_secs(20);
    let (kept, _) = read_by_bo(typing(typed, false));
    assert!(
        kept.iter().all(|(at, typing)| *at >= typed || *typing),
        "typing for 20 s reads as typing throughout"
    );
    let last_key = typed - Duration::from_millis(250);
    assert!(
        kept.iter()
            .all(|(at, typing)| !*typing || *at < last_key + COMPOSING_LINGER),
        "a draft left in place stops reading as typing within the linger"
    );
    let (sent, _) = read_by_bo(typing(typed, true));
    assert!(
        sent.iter().all(|(at, typing)| (*at < typed) == *typing),
        "typing until sent, and not a moment after"
    );
}

/// A field whose Enter sends what it holds into the channel, as a native app builds its
/// composer, or into an effect of the device's, as a search box or a setting does.
fn field(kind: tilo_actions::OperationKind) -> crate::widget::TextInput {
    use tilo_actions::{ActionBinding, ActionSpec, ResourceSpec};
    let send = match kind {
        tilo_actions::OperationKind::Document => ActionSpec::prompted_param(
            "send",
            "Send",
            tilo_actions::ActionParam::required("text", tilo_actions::ParamType::Text, "Text"),
            "Message",
            tilo_actions::ExecutionLocus::ClientLocal,
            |text| tilo_actions::EncodedAction::encode_operation("chat.send", &text).expect("text"),
        ),
        kind => ActionSpec::operation("send", "Send", kind, "chat.search", &()).expect("unit"),
    };
    let mut channel =
        ResourceSpec::new("channel-1", "TextChannel", "General").with_action(send.clone());
    channel.app_id = Some("so.tilo.chat".to_owned());
    let binding = ActionBinding::new(&channel, &send, |text: String| {
        serde_json::Map::from_iter([("text".to_owned(), text.into())])
    })
    .expect("the channel publishes send");
    let mut field = crate::widget::TextInput::new(Vec::new()).on_submit_action(binding);
    tilo_ui_base::widget::Widget::set_focus(&mut field, true);
    field
}

#[test]
fn a_native_composer_reads_as_typing_while_its_person_types() {
    let typed = Duration::from_secs(20);
    let mut text = String::new();
    let mut composer = field(tilo_actions::OperationKind::Document);
    let (read, writing) = read_by_bo(|at| {
        if at < typed && at.as_millis() % 300 == 0 {
            text.push('a');
            composer.set_text(&text);
        }
        if at == typed {
            composer.set_text("");
        }
        capability::composing_draft(&composer)
    });
    assert!(
        read.iter().all(|(at, typing)| (*at < typed) == *typing),
        "typing in the composer reads as typing until it is sent, and not a moment after"
    );
    let resaid = typed.as_secs() / (COMPOSING_LINGER / 2).as_secs();
    assert!(
        writing >= usize::try_from(resaid).expect("small"),
        "each keystroke after half the linger says so again: {writing}"
    );

    composer.set_text("left here");
    tilo_ui_base::widget::Widget::set_focus(&mut composer, false);
    assert_eq!(
        capability::composing_draft(&composer),
        None,
        "a field left is no draft"
    );
    for kind in [
        tilo_actions::OperationKind::Effect,
        tilo_actions::OperationKind::View,
    ] {
        let mut search = field(kind);
        search.set_text("hello");
        assert_eq!(
            capability::composing_draft(&search),
            None,
            "a field whose Enter does not write a shared record is no draft"
        );
    }
}

/// Ada starts typing and her window vanishes without saying she stopped: Bo's window repaints
/// as she starts, sleeps, and wakes by itself once the linger ends to repaint without her.
#[test]
fn a_readers_window_wakes_to_clear_typing_gone_quiet() {
    use crate::time::testing::with_monotonic_time as at;
    let (start, ada) = (crate::time::now(), MemberId::person("ada"));
    let ends = start + COMPOSING_LINGER;
    let mut bo = crate::headless::Driver::for_root(
        crate::state::State::new(),
        crate::widget::TextInput::local(),
        (400, 80),
        tilo_ui_base::Theme::dark(),
    );
    at(start, || while bo.poll_frame().expect("frame") {});
    let quiet = bo.presence().widget_presence(start);
    at(start, || {
        bo.update_presence(|presence| {
            let typing = PresencePayload::Composing { composing: true };
            presence.apply_remote_presence_at(ada.clone(), channel(), typing, start);
            true
        });
        assert!(bo.poll_frame().expect("frame"), "her start is a frame");
        assert_eq!(bo.frame_schedule().next_wake_deadline, Some(ends));
    });
    // What widgets paint from changed both times, so the window repaints whole.
    assert_ne!(bo.presence().widget_presence(start), quiet);
    let nothing_due = at(ends - Duration::from_millis(1), || bo.frame_schedule());
    assert!(!nothing_due.needs_redraw);
    at(ends, || {
        assert!(bo.poll_frame().expect("frame"));
        assert!(bo.presence().composing_at(&channel(), ends).is_empty());
        assert_eq!(bo.presence().widget_presence(ends), quiet);
        assert_eq!(bo.frame_schedule().next_wake_deadline, None);
    });
}
