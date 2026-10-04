#![allow(clippy::unwrap_used, reason = "test assertions")]

//! What the host gives a window: which device it runs on (views, surfaces
//! and a new record's seed read it, an action that records it carries it),
//! and file areas that stay in a test or capture profile.

use std::cell::RefCell;

use tilo_app::prelude::*;

thread_local! {
    /// The file areas the last view was given.
    static FILES: RefCell<Option<tilo_app::storage::AppFiles>> = const { RefCell::new(None) };
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Runs {
    pub devices: Vec<String>,
}

#[resource(kind = "test.runs", label = "Runs")]
impl Runs {
    /// Record a run on `device`.
    #[action]
    pub fn run_on(&mut self, device: String) {
        self.devices.push(device);
    }
}

struct RunsApp;

impl ResourceApp for RunsApp {
    type Resource = Runs;
    const ID: &'static str = "so.tilo.test-runs";
    const TITLE: &'static str = "Runs";

    fn view(cx: &mut ViewCx<Runs>) -> Element {
        FILES.set(Some(cx.storage().files()));
        let device = cx.device().unwrap_or("nowhere").to_owned();
        let shown = format!("on {device}");
        Box::new(
            Column::new()
                .push(Box::new(Text::new(&shown)))
                .push(Box::new(cx.button("Run here", Runs::RUN_ON.with(device)))),
        )
    }

    fn seed(cx: &SeedCx<'_>) -> Vec<Invoke<Runs>> {
        vec![Runs::RUN_ON.with(format!("seeded on {}", cx.device()))]
    }
}

#[test]
fn a_window_knows_its_device_and_an_action_carries_it() {
    let mut app = TestApp::<RunsApp>::new().unwrap();
    assert_eq!(app.state().devices, ["seeded on test-device"]);
    app.harness().click_named("run-on").unwrap();
    assert_eq!(
        app.state().devices,
        ["seeded on test-device", "test-device"]
    );

    // A past version is shown on no device in particular.
    let first = app.versions().first().unwrap().hash;
    let past = app.past_version(&[first]).unwrap();
    assert!(past.texts().iter().any(|text| text == "on nowhere"));
}

#[test]
fn a_test_profile_keeps_the_apps_files_out_of_the_persons() {
    let app = TestApp::<RunsApp>::new().unwrap();
    let given = FILES.take().unwrap();
    assert_eq!(given, app.files(), "the view writes the profile's files");
    assert_ne!(
        given,
        tilo_app::storage::AppFiles::for_app(RunsApp::ID),
        "never the person's"
    );
}
