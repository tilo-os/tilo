//! A reader's window wakes by itself to clear a typing state that went quiet, and only at the
//! moment the linger ends.
use ::collaboration::PresencePayload;
use tilo_ui_app::State;
use tilo_ui_app::headless::Driver;
use tilo_ui_app::time::Duration;
use tilo_ui_app::time::testing::with_monotonic_time as at;
use tilo_ui_app::widget::TextInput;
use tilo_ui_base::permissions::MemberId;
use tilo_ui_base::presence::COMPOSING_LINGER;
use tilo_ui_base::presence::PresenceKey;

/// Ada and Bo's shared channel, as the draft the tests drive says it.
fn channel() -> PresenceKey {
    PresenceKey::resource("so.tilo.chat", "channel-1", "TextChannel")
}

/// Ada starts typing and her window vanishes without saying she stopped: Bo's window repaints
/// as she starts, sleeps, and wakes by itself once the linger ends to repaint without her.
#[test]
fn a_readers_window_wakes_to_clear_typing_gone_quiet() {
    let (start, ada) = (tilo_ui_app::time::now(), MemberId::person("ada"));
    let ends = start + COMPOSING_LINGER;
    let mut bo = Driver::for_root(
        State::new(),
        TextInput::local(),
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
