//! "Add a device": the sheet as a person meets it for a workspace no host on
//! the web keeps — the ticket this computer issued, and where to type it.

use std::rc::Rc;

use tilo_sheets::pair_device::{self, AddDevice, PairDeviceEffect};
use ui::headless::FrameHarness;

const VIEWPORT: (u32, u32) = (1280, 800);

const TICKET: &str = "tilo1qqqsyqcyq5rqwzqfpg9scrgwpugpzysnzs23v9ccrydpk8qarc0jq";

fn frame_says(frame: &FrameHarness, text: &str) -> bool {
    frame.a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
            || node.value().is_some_and(|value| value.contains(text))
    })
}

/// Whether the sheet built from `device` says `text`.
fn says(device: &AddDevice, text: &str) -> bool {
    let ask: pair_device::Ask = Rc::new(|_: PairDeviceEffect| {});
    let link = ui::LocalState::new(device.link.clone());
    let inputs = ui::headless::fixture_runner_inputs();
    let mut frame = FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        pair_device::sheet(device, &link, &ask, None),
        VIEWPORT,
        inputs.theme,
        inputs.text_renderer,
        ui::collaboration::local_user_id(),
    );
    frame.relayout();
    frame_says(&frame, text)
}

tilo_test_env::scenario! {
    /// The sheet shows this computer's ticket with where the new computer takes
    /// it, and says it is asking while there is none yet.
    fn a_ticket_is_shown_with_where_to_type_it;

    phase the_ticket_is_shown_with_its_hint_and_copy;
    {
        let device = AddDevice::ticket(TICKET, 9);
        assert!(says(&device, TICKET), "{device:?}");
        assert!(says(&device, &device.in_tilo), "{device:?}");
        assert!(says(&device, &device.copy), "{device:?}");
        assert!(says(&device, &device.lasting), "{device:?}");
        assert_ne!(device.copy, AddDevice::of(TICKET, 9).copy, "a ticket is no link");
    }

    phase while_asking_the_sheet_says_so_and_shows_no_code;
    {
        let device = AddDevice::asking();
        assert!(says(&device, device.refusal.as_deref().unwrap_or_default()), "{device:?}");
        assert!(!says(&device, &AddDevice::ticket(TICKET, 9).copy), "{device:?}");
    }
}
