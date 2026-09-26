//! The toolkit's own entry points (clipboard verbs, external links, the file
//! chooser, screen-reader speech) resolve through the current host's
//! services, so a host that records effects sees every one of them.

use tilo_ui::a11y::{AccessNode, Role};
use tilo_ui::clipboard::ClipboardTarget;
use tilo_ui::host::{self, HostServices};
use tilo_ui::portal::{PickRequest, PortalOutcome};

#[test]
fn toolkit_entry_points_reach_the_recording_host() {
    let (services, recorder) = HostServices::recording();
    recorder.respond_to_pick(PortalOutcome::Opened("/tmp/picked.txt".into()));
    host::scoped(&services, || {
        tilo_ui::text_selection::clipboard_copy("copied");
        assert_eq!(
            tilo_ui::text_selection::clipboard_paste().as_deref(),
            Some("copied")
        );
        assert!(tilo_ui::widget::try_open_external_url("https://tilo.so").is_ok());
        let picked = PickRequest::open("Open", "Open").send().recv();
        assert_eq!(picked, Ok(PortalOutcome::Opened("/tmp/picked.txt".into())));
        let mut reader = tilo_ui::screen_reader::ScreenReader::new();
        reader.announce_focus(&AccessNode::new(Role::Button).with_name("Save"));
    });
    assert_eq!(
        recorder.copies(),
        [(ClipboardTarget::Clipboard, "copied".to_string())]
    );
    assert_eq!(recorder.opened_uris(), ["https://tilo.so"]);
    assert_eq!(recorder.pick_requests().len(), 1);
    assert!(
        recorder
            .utterances()
            .iter()
            .any(|utterance| utterance.text.contains("Save"))
    );
}
