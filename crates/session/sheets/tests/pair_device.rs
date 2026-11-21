//! "Add a device": the sheet as a person meets it while a machine asks to
//! join over the internet — what the machine is, and the answers offered.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::rc::Rc;

use tilo_sheets::pair_device::{self, AddDevice, PairDeviceEffect, WaitingMachine};
use ui::headless::FrameHarness;

const VIEWPORT: (u32, u32) = (1280, 800);

fn frame_says(frame: &FrameHarness, text: &str) -> bool {
    frame.a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
            || node.value().is_some_and(|value| value.contains(text))
    })
}

/// Whether the sheet built from `device` and `typed` says `text`.
fn says(device: &AddDevice, typed: &str, text: &str) -> bool {
    let ask: pair_device::Ask = Rc::new(|_: PairDeviceEffect| {});
    let link = ui::LocalState::new(device.link.clone());
    let typed = ui::LocalState::new(typed.to_owned());
    let inputs = ui::headless::fixture_runner_inputs();
    let mut frame = FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        pair_device::sheet(device, &link, &typed, &ask, None),
        VIEWPORT,
        inputs.theme,
        inputs.text_renderer,
        ui::collaboration::local_user_id(),
    );
    frame.relayout();
    frame_says(&frame, text)
}

fn machine() -> WaitingMachine {
    WaitingMachine {
        ask: "ab".repeat(16),
        label: "Home server".into(),
        code: "482913".into(),
    }
}

tilo_test_env::scenario! {
    /// The sheet lists the machine that asked to join with what it is and the
    /// code both sides show, offers the answers, and takes a machine's own line.
    fn a_machine_waiting_to_join_is_shown_with_its_answers;

    phase a_waiting_machine_is_shown_with_what_it_is;
    {
        let mut device = AddDevice::of("https://tilo.coffee/#/@eu1/w/ws?pair=abab", 9);
        device.waiting = vec![machine()];
        assert!(says(&device, "", "Home server"), "{device:?}");
        assert!(!says(&device, "", &device.waiting_nothing), "{device:?}");
        assert!(!says(&device, "", &device.alt), "{device:?}");
        assert!(says(&device, "", "482913"), "{device:?}");
        assert!(says(&device, "", "Approve Home server with code 482913"), "{device:?}");
        assert!(says(&device, "", "Deny Home server"), "{device:?}");
    }

    phase a_sheet_with_no_machine_says_so_and_teaches_the_line;
    {
        let device = AddDevice::of("https://tilo.coffee/#/@eu1/w/ws?pair=abab", 9);
        assert!(says(&device, "", &device.waiting_nothing), "{device:?}");
        assert!(says(&device, "", &device.link_field), "{device:?}");
        assert!(says(&device, "tilo://enroll?endpoint=ab&code=1", &device.link_use), "{device:?}");
    }
}
