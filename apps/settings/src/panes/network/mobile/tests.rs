use super::*;

const PIN: &str = "1234";
const PUK: &str = "12345678";

fn cx() -> crate::area::AreaContext {
    let profile = tempfile::tempdir().unwrap().keep();
    crate::area::AreaContext::in_profile(profile)
}

fn scripted(sample: Option<MobileSample>) -> (Arc<ScriptedModem>, MobileModel) {
    let device = Arc::new(ScriptedModem::new(sample));
    let model = MobileModel::loaded_from(
        Reach::of(&cx()),
        Arc::clone(&device) as Arc<dyn MobileService>,
    );
    (device, model)
}

/// Begin `effect`, carry out its work as the worker would, and take in
/// the read that follows a change.
fn run(model: &mut MobileModel, effect: MobileEffect) -> Result<(), String> {
    let landed = match model.begin(effect)? {
        Begun::Done => return Ok(()),
        Begun::Work(work) => work(),
    };
    let answer = model.land(landed).map(|_| ());
    let snapshot = model.service.snapshot();
    model.adopt(snapshot);
    answer
}

/// What `read` finds in a Settings window on the modem's page over `model`
/// as it is now. The model is lent to the window's Mobile area meanwhile
/// and handed back as the page left it.
fn on_its_page<T>(model: &mut MobileModel, read: impl FnOnce(&dyn ui::widget::Widget) -> T) -> T {
    let mut app = App::new().unwrap();
    // Away from Network, so its page is built over the model lent.
    crate::effects::show_pane(&mut app, crate::categories::Category::Appearance);
    let held = app
        .with_widget::<crate::window::Inspected, _>(|settings| {
            Rc::clone(area(settings).unwrap().model())
        })
        .unwrap();
    std::mem::swap(&mut *held.borrow_mut(), model);
    crate::effects::show_pane(&mut app, crate::categories::Category::Network);
    app.with_widget::<crate::window::Inspected, _>(|settings| {
        super::super::area(settings)
            .unwrap()
            .model()
            .borrow_mut()
            .open_page(super::super::PAGE_MOBILE, t("net-mobile"));
    })
    .unwrap();
    app.settle();
    let found = read(app.harness().root_widget());
    std::mem::swap(&mut *held.borrow_mut(), model);
    found
}

/// What a Settings window shows on the modem's page over `model`.
fn said(model: &mut MobileModel) -> Vec<String> {
    on_its_page(model, provider_settings_ui::widgets::announced_text)
}

fn says(model: &mut MobileModel, text: &str) -> bool {
    said(model).iter().any(|said| said.contains(text))
}

#[test]
fn without_a_modem_there_is_no_section_and_no_mobile_setting_is_tried() {
    let (device, mut model) = scripted(None);
    assert!(model.loaded);
    assert!(!model.present());
    assert!(
        on_its_page(&mut model, |page| {
            ui::widget::find_test_id(page, "network.mobile").is_none()
        }),
        "nothing is shown"
    );
    for effect in [
        MobileEffect::SetMobileBroadband { on: true },
        MobileEffect::SetDataRoaming { on: true },
        MobileEffect::SetPreferredNetwork {
            network: "4g".into(),
        },
    ] {
        assert_eq!(
            run(&mut model, effect).unwrap_err(),
            "No mobile broadband modem on this device"
        );
    }
    assert!(device.changes().is_empty(), "nothing reached the device");
}

type App = ui::framework::testing::TestApp<crate::SettingsShell>;

/// The availability Settings publishes for `id`.
fn published(app: &mut App, id: &str) -> ui::tilo_actions::Availability {
    app.actions()
        .expect("the actions are published")
        .into_iter()
        .find(|action| action.address.action == id)
        .unwrap_or_else(|| panic!("`{id}` is published"))
        .contract
        .availability
}

fn unavailable(code: &str, reason: &str) -> ui::tilo_actions::Availability {
    ui::tilo_actions::Availability::Unavailable {
        code: code.into(),
        reason: reason.into(),
    }
}

const SETTINGS: [&str; 5] = [
    "set-mobile-broadband",
    "set-data-roaming",
    "set-mobile-apn",
    "set-preferred-network",
    "change-sim-pin",
];

#[test]
fn mobile_settings_follow_the_kernel_until_read_then_modem_manager() {
    use crate::radios::Present;
    use ui::tilo_actions::Availability;

    let mut app = App::new().unwrap();
    let radios = app
        .with_widget::<crate::window::Inspected, _>(|settings| settings.radios().clone())
        .expect("the Settings window is mounted");
    radios.set(Present::NONE);
    app.settle();
    let no_modem = unavailable("no_modem", "No mobile broadband modem on this device");
    for id in SETTINGS {
        assert_eq!(published(&mut app, id), no_modem, "`{id}`");
    }
    assert_eq!(
        published(&mut app, "refresh-mobile"),
        Availability::Available
    );

    // Before the modems are read, the kernel listing one offers it.
    let cx = cx();
    let unread = MobileArea::new(&cx);
    assert!(unread.statuses().is_empty(), "nothing has said yet");
    cx.radios.set(Present::NONE);
    assert!(!unread.statuses().is_empty(), "the kernel lists no modem");
    cx.radios.set(Present::ALL);
    assert!(
        unread.statuses().is_empty(),
        "a modem the kernel lists is offered before it is read"
    );

    radios.set(Present::ALL);
    // Once read, ModemManager's answer is the one: the scripted device
    // has no modem, whatever the kernel says.
    app.with_widget::<crate::window::Inspected, _>(|settings| {
        area(settings).unwrap().model();
    })
    .unwrap();
    app.settle();
    for id in SETTINGS {
        assert_eq!(published(&mut app, id), no_modem, "`{id}`");
    }
}

#[test]
fn only_a_locked_sim_offers_unlocking_and_only_a_blocked_one_the_puk() {
    use ui::tilo_actions::Availability;

    let mut app = App::new().unwrap();
    let (_, locked) = scripted(Some(MobileSample::Locked));
    app.with_widget::<crate::window::Inspected, _>(|settings| {
        *area(settings).unwrap().model().borrow_mut() = locked;
    })
    .unwrap();
    app.settle();
    assert_eq!(published(&mut app, "unlock-sim"), Availability::Available);
    assert_eq!(
        published(&mut app, "unblock-sim"),
        unavailable("not_blocked", "No SIM is blocked")
    );
    assert_eq!(
        published(&mut app, "set-data-roaming"),
        Availability::Available
    );

    let (_, registered) = scripted(Some(MobileSample::Registered));
    app.with_widget::<crate::window::Inspected, _>(|settings| {
        *area(settings).unwrap().model().borrow_mut() = registered;
    })
    .unwrap();
    app.settle();
    assert_eq!(
        published(&mut app, "unlock-sim"),
        unavailable("not_locked", "No SIM asks for its PIN")
    );
}

#[test]
fn a_registered_modem_shows_operator_technology_signal_and_data() {
    let (_, mut model) = scripted(Some(MobileSample::Registered));
    assert!(model.present());
    assert!(model.data_on());
    let said = said(&mut model);
    for expected in ["Telekom.de", "LTE, connected", "Data roaming", "Data used"] {
        assert!(
            said.iter().any(|text| text.contains(expected)),
            "`{expected}` in {said:?}"
        );
    }
    assert!(
        said.iter().any(|text| text == "Vodafone.de"),
        "both SIMs are listed"
    );
    assert!(
        said.iter().any(|text| text.contains("eSIM")),
        "the eSIM is marked"
    );
    let inspection = model.inspection();
    assert_eq!(inspection["modems"][0]["registration"], "home");
    assert_eq!(inspection["modems"][0]["technology"], "LTE");
    assert_eq!(inspection["data"], true);
}

#[test]
fn roaming_is_said_and_switched_on_the_profile() {
    let (device, mut model) = scripted(Some(MobileSample::Roaming));
    assert!(says(&mut model, "Orange F"));
    assert!(says(&mut model, "roaming"));
    run(&mut model, MobileEffect::SetDataRoaming { on: false }).unwrap();
    assert_eq!(model.profile.as_ref().map(|p| p.roaming), Some(false));
    assert!(
        device
            .changes()
            .iter()
            .any(|change| change.contains("Roaming")),
        "{:?}",
        device.changes()
    );
}

/// The window over a scripted modem standing as `sample`, its Network
/// pane open on the first page.
fn hosted(sample: MobileSample) -> (Arc<ScriptedModem>, App) {
    let device = Arc::new(ScriptedModem::new(Some(sample)));
    let service = Arc::clone(&device) as Arc<dyn MobileService>;
    crate::area::replace_in_next_window(move |context| MobileArea::over(context, service));
    let mut app = App::new().unwrap();
    app.harness().resize((1280, 900)).unwrap();
    app.call("open-pane", serde_json::json!({"pane": "network"}))
        .unwrap();
    app.settle();
    (device, app)
}

/// Settle `app` until `done` says the modem's model is where the test
/// waits for it.
fn settle_until(app: &mut App, done: impl Fn(&MobileModel) -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        let reached = app
            .with_widget::<crate::window::Inspected, _>(|settings| {
                done(&area(settings).unwrap().model().borrow())
            })
            .unwrap();
        if reached {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the modem work never landed"
        );
        std::thread::yield_now();
    }
}

/// What the window shows, in reading order.
fn shown(app: &mut App) -> Vec<String> {
    provider_settings_ui::widgets::announced_text(app.harness().root_widget())
}

fn lock(app: &mut App) -> Option<SimLock> {
    app.with_widget::<crate::window::Inspected, _>(|settings| {
        area(settings)
            .unwrap()
            .model()
            .borrow()
            .modem()
            .map(|modem| modem.lock)
    })
    .unwrap()
}

/// Answer the open prompt: `codes` typed into its fields in order.
fn answer(app: &mut App, codes: &[&str]) {
    for (at, code) in codes.iter().enumerate() {
        if at > 0 {
            app.harness().key("Tab");
        }
        app.harness().type_text(code);
    }
    app.harness().key("Return");
}

/// The first page shows the modem next to Wi-Fi: how it stands with the
/// signal meter, its data switch, and the way to its page; a device
/// without a modem shows no Mobile card.
#[test]
fn the_first_page_shows_the_modem_next_to_wifi() {
    let (_, mut app) = hosted(MobileSample::Registered);
    let said = shown(&mut app);
    let at = |text: &str| said.iter().position(|said| said.contains(text));
    let mobile = at("Telekom.de").unwrap_or_else(|| panic!("the modem in {said:?}"));
    assert!(at("Wi-Fi").is_some_and(|wifi| wifi < mobile), "{said:?}");
    assert!(
        !said.iter().any(|text| text.contains("Data roaming")),
        "the rest is on its page"
    );

    app.call("show-mobile", serde_json::json!({})).unwrap();
    app.settle();
    let said = shown(&mut app);
    assert!(
        said.iter().any(|text| text.contains("Data roaming")),
        "{said:?}"
    );

    let mut app = App::new().unwrap();
    app.call("open-pane", serde_json::json!({"pane": "network"}))
        .unwrap();
    app.settle();
    assert!(!shown(&mut app).iter().any(|text| text == "Mobile"));
}

/// A locked SIM asks for its PIN with the window's password prompt: a
/// wrong PIN is said there with one try fewer, the right one unlocks it
/// and closes the prompt. No code is drawn, inspected or recorded.
#[test]
fn a_locked_sim_is_unlocked_from_the_password_prompt() {
    let (device, mut app) = hosted(MobileSample::Locked);
    assert!(
        shown(&mut app)
            .iter()
            .any(|text| text.contains("2 tries left."))
    );
    app.call("unlock-sim", serde_json::json!({})).unwrap();
    settle_until(&mut app, |model| {
        model.asked() == Some(&codes::CodeAsk::Unlock(None))
    });

    answer(&mut app, &["0000"]);
    settle_until(&mut app, |model| {
        model.in_flight == 0 && model.modem().and_then(|modem| modem.retries.pin) == Some(1)
    });
    assert!(
        app.with_widget::<crate::window::Inspected, _>(|settings| area(settings)
            .unwrap()
            .model()
            .borrow()
            .asking
            .is_some())
            .unwrap(),
        "a wrong PIN asks again"
    );
    assert_eq!(lock(&mut app), Some(SimLock::Pin));

    answer(&mut app, &[PIN]);
    settle_until(&mut app, |model| {
        model.asking.is_none() && model.in_flight == 0
    });
    assert_eq!(lock(&mut app), Some(SimLock::None));
    let said = shown(&mut app);
    assert!(
        said.iter().any(|text| text.contains("SIM unlocked.")),
        "{said:?}"
    );
    for code in ["0000", PIN] {
        assert!(!said.iter().any(|text| text.contains(code)));
        assert!(!device.changes().iter().any(|change| change.contains(code)));
        let inspection = app
            .with_widget::<crate::window::Inspected, _>(|settings| {
                area(settings)
                    .unwrap()
                    .model()
                    .borrow()
                    .inspection()
                    .to_string()
            })
            .unwrap();
        assert!(!inspection.contains(code));
    }
}

/// The last wrong PIN blocks the SIM, and the prompt asks for its PUK and
/// a new PIN next; they unblock it.
#[test]
fn the_last_wrong_pin_asks_for_the_puk_and_a_new_pin() {
    let (_, mut app) = hosted(MobileSample::Locked);
    app.call("unlock-sim", serde_json::json!({})).unwrap();
    settle_until(&mut app, |model| model.asking.is_some());
    for tried in 1..=2 {
        answer(&mut app, &["9999"]);
        settle_until(&mut app, |model| {
            model.in_flight == 0
                && model.modem().and_then(|modem| modem.retries.pin) == Some(2 - tried)
        });
    }
    settle_until(&mut app, |model| {
        model.asked() == Some(&codes::CodeAsk::Unblock(None))
    });
    assert_eq!(lock(&mut app), Some(SimLock::Puk));

    answer(&mut app, &["00000000", "4321", "4321"]);
    settle_until(&mut app, |model| {
        model.in_flight == 0 && model.modem().and_then(|modem| modem.retries.puk) == Some(9)
    });
    assert_eq!(
        lock(&mut app),
        Some(SimLock::Puk),
        "a wrong PUK unblocks nothing"
    );

    answer(&mut app, &[PUK, "4321", "4321"]);
    settle_until(&mut app, |model| {
        model.asking.is_none() && model.in_flight == 0
    });
    assert_eq!(lock(&mut app), Some(SimLock::None));
}

/// The PUK sample opens on the modem's page with the prompt asking for the
/// PUK, as the capture shows it.
#[test]
fn the_puk_sample_starts_blocked() {
    let (_, mut model) = scripted(Some(MobileSample::Puk));
    assert_eq!(model.modem().unwrap().lock, SimLock::Puk);
    assert!(says(&mut model, "SIM blocked"));
    assert!(says(&mut model, "Unblock SIM…"));
}

/// Turning the PIN lock off asks for the PIN first; nothing is sent until
/// it is given, and Cancel leaves the lock as it was.
#[test]
fn turning_the_pin_lock_off_asks_for_the_pin_first() {
    let (device, mut app) = hosted(MobileSample::Registered);
    let ask = |app: &mut App| {
        app.call("set-sim-pin-lock", serde_json::json!({"on": false}))
            .unwrap();
        settle_until(app, |model| {
            model.asked()
                == Some(&codes::CodeAsk::PinLock {
                    sim: None,
                    on: false,
                })
        });
    };
    ask(&mut app);
    assert!(
        device.changes().is_empty(),
        "nothing is sent without the PIN"
    );
    app.harness().key("Escape");
    settle_until(&mut app, |model| model.asking.is_none());
    assert!(device.changes().is_empty(), "a cancel sends nothing");

    ask(&mut app);
    answer(&mut app, &[PIN]);
    settle_until(&mut app, |model| {
        model.asking.is_none() && model.modem().unwrap().pin_enabled == Some(false)
    });
}

/// Changing the PIN asks for the current and a new one; a wrong current
/// PIN is said and asked again.
#[test]
fn changing_the_pin_asks_for_the_current_one_and_a_new_one() {
    let (_, mut app) = hosted(MobileSample::Registered);
    app.call("change-sim-pin", serde_json::json!({})).unwrap();
    settle_until(&mut app, |model| {
        model.asked() == Some(&codes::CodeAsk::ChangePin(None))
    });
    answer(&mut app, &["0000", "5678"]);
    settle_until(&mut app, |model| {
        model.in_flight == 0 && model.modem().and_then(|modem| modem.retries.pin) == Some(2)
    });
    assert!(
        app.with_widget::<crate::window::Inspected, _>(|settings| area(settings)
            .unwrap()
            .model()
            .borrow()
            .asking
            .is_some())
            .unwrap()
    );
    answer(&mut app, &[PIN, "5678"]);
    settle_until(&mut app, |model| {
        model.asking.is_none() && model.in_flight == 0
    });
    assert!(
        shown(&mut app)
            .iter()
            .any(|text| text.contains("PIN changed."))
    );
}

/// Codes given to the effects change the SIM at once, as an agent gives
/// them; a wrong one is refused with what went wrong.
#[test]
fn codes_given_to_the_effects_change_the_sim_directly() {
    let (_, mut model) = scripted(Some(MobileSample::Locked));
    let wrong = run(
        &mut model,
        MobileEffect::UnlockSim {
            sim: None,
            pin: Some("0000".into()),
        },
    );
    assert_eq!(wrong.unwrap_err(), "Wrong PIN");
    run(
        &mut model,
        MobileEffect::UnlockSim {
            sim: None,
            pin: Some(PIN.into()),
        },
    )
    .unwrap();
    assert_eq!(model.modem().unwrap().lock, SimLock::None);
    let wrong = run(
        &mut model,
        MobileEffect::ChangeSimPin {
            sim: None,
            old_pin: Some("0000".into()),
            new_pin: Some("5678".into()),
        },
    );
    assert_eq!(wrong.unwrap_err(), "Wrong PIN");
    run(
        &mut model,
        MobileEffect::ChangeSimPin {
            sim: None,
            old_pin: Some(PIN.into()),
            new_pin: Some("5678".into()),
        },
    )
    .unwrap();
    assert!(says(&mut model, "PIN changed."));
    assert!(model.asking.is_none(), "nothing was asked");
}

#[test]
fn the_network_type_is_chosen_by_its_key_or_its_words() {
    let (device, mut model) = scripted(Some(MobileSample::Registered));
    run(
        &mut model,
        MobileEffect::SetPreferredNetwork {
            network: "4g".into(),
        },
    )
    .unwrap();
    assert_eq!(
        model
            .modem()
            .unwrap()
            .modes
            .map(NetworkModes::key)
            .as_deref(),
        Some("4g")
    );
    run(
        &mut model,
        MobileEffect::SetPreferredNetwork {
            network: "2G to 4G, 4G preferred".into(),
        },
    )
    .unwrap();
    assert_eq!(
        model
            .modem()
            .unwrap()
            .modes
            .map(NetworkModes::preferred_generation),
        Some(Some(4))
    );
    assert!(
        run(
            &mut model,
            MobileEffect::SetPreferredNetwork {
                network: "6g".into(),
            },
        )
        .is_err()
    );
    assert_eq!(device.changes().len(), 2);
}

#[test]
fn the_apn_is_automatic_or_a_name_never_empty() {
    let (device, mut model) = scripted(Some(MobileSample::Registered));
    run(&mut model, MobileEffect::ShowApnEditor).unwrap();
    assert!(model.apn_automatic.get());
    model.apn_automatic.set(false);
    assert_eq!(
        run(
            &mut model,
            MobileEffect::SetMobileApn {
                automatic: None,
                apn: None,
            },
        )
        .unwrap_err(),
        "Enter an access point name, or choose it automatically."
    );
    model.apn_text.set(" internet.telekom ".into());
    run(
        &mut model,
        MobileEffect::SetMobileApn {
            automatic: None,
            apn: None,
        },
    )
    .unwrap();
    assert_eq!(
        model
            .profile
            .as_ref()
            .and_then(|p| p.apn.clone())
            .as_deref(),
        Some("internet.telekom")
    );
    assert!(!model.apn_open);
    run(
        &mut model,
        MobileEffect::SetMobileApn {
            automatic: Some(true),
            apn: None,
        },
    )
    .unwrap();
    assert_eq!(model.profile.as_ref().and_then(|p| p.apn.clone()), None);
    assert_eq!(device.changes().len(), 2);
}

#[test]
fn metering_and_data_follow_the_profile() {
    let (_, mut model) = scripted(Some(MobileSample::Registered));
    run(
        &mut model,
        MobileEffect::SetMobileMetered {
            metered: "unmetered".into(),
        },
    )
    .unwrap();
    assert_eq!(
        model.profile.as_ref().map(|p| p.metered),
        Some(MeteredPolicy::Unmetered)
    );
    run(
        &mut model,
        MobileEffect::SetMobileMetered {
            metered: "Metered".into(),
        },
    )
    .unwrap();
    assert_eq!(
        model.profile.as_ref().map(|p| p.metered),
        Some(MeteredPolicy::Metered)
    );
    run(&mut model, MobileEffect::SetMobileBroadband { on: false }).unwrap();
    assert!(!model.data_on());
    run(&mut model, MobileEffect::SetMobileBroadband { on: true }).unwrap();
    assert!(model.data_on());
}

#[test]
fn a_change_the_device_refuses_is_said() {
    let (device, mut model) = scripted(Some(MobileSample::Registered));
    device.refuse("nmcli is not installed");
    let error = run(&mut model, MobileEffect::SetDataRoaming { on: true }).unwrap_err();
    assert!(error.contains("nmcli is not installed"), "{error}");
    assert!(says(&mut model, "nmcli is not installed"));
}

#[test]
fn the_other_sim_is_used_by_its_slot() {
    let (_, mut model) = scripted(Some(MobileSample::Registered));
    run(
        &mut model,
        MobileEffect::UseSim {
            sim: "Vodafone.de".into(),
        },
    )
    .unwrap();
    let switched = model.modem().unwrap();
    assert_eq!(switched.active_sim().map(|sim| sim.slot), Some(Some(2)));
    assert!(
        run(
            &mut model,
            MobileEffect::UseSim {
                sim: "Nobody".into(),
            },
        )
        .is_err()
    );
}

/// One modem's traffic total, as the session counts it.
fn counted(total: u64, at: i64) -> serde_json::Value {
    serde_json::json!({
        "readings": [{
            "counter": "modem:0", "connection": "mobile:8931", "kind": "mobile",
            "name": "Telia", "received": total, "sent": 0,
        }],
        "boot": "b", "at": at,
        "offset": ui::clock::local_now().offset().local_minus_utc(),
    })
}

/// The count is the device's: the session counts the modem's totals into
/// the section through its verb with no page ever opened, and an agent
/// reading the section sees them. Reading the modem in Settings counts
/// nothing of its own.
#[test]
fn the_count_is_kept_in_the_section_with_no_page_opened() {
    use crate::testing::invoke_section;
    let kind = <NetworkUsage as ui::tilo_resource::Resource>::KIND;
    // The window opens on About: the network page, whose model reads the
    // modem, is never built.
    let mut app =
        ui::prelude::TestApp::<crate::resource::SettingsShell>::launched(&["settings://about"])
            .unwrap();
    let now = tilo_clock::unix_now_i64();
    for (total, at) in [(9_000_u64, now), (9_750, now + 1)] {
        invoke_section(&mut app, kind, "count-data-used", counted(total, at)).unwrap();
    }
    let usage = app.settings::<NetworkUsage>().get();
    let today = ui::clock::local_now().date_naive();
    assert_eq!(usage.connections["mobile:8931"].on(today).received, 750);
    assert_eq!(usage.counters["modem:0"].received, 9_750);
    let opened = app
        .with_widget::<crate::window::Inspected, _>(|settings| settings.selected())
        .unwrap();
    assert_eq!(
        opened,
        crate::categories::Category::About,
        "no modem page was opened"
    );
}

/// The reset day and the plan are the section's fields, bound on the
/// modem's page, the reset day refused unless every month has it;
/// resetting is the section's verb.
#[test]
fn the_reset_day_limit_and_reset_are_the_sections() {
    use crate::testing::{edit_field, invoke_section};
    let kind = <NetworkUsage as ui::tilo_resource::Resource>::KIND;
    let (_, mut app) = hosted(MobileSample::Registered);
    let now = tilo_clock::unix_now_i64();
    invoke_section(&mut app, kind, "count-data-used", counted(1_000, now)).unwrap();
    invoke_section(&mut app, kind, "count-data-used", counted(1_500, now)).unwrap();
    let today = ui::clock::local_now().date_naive();
    assert_eq!(app.settings::<NetworkUsage>().get().mobile_used(today), 500);

    app.call("show-mobile", serde_json::json!({})).unwrap();
    app.settle();
    for id in [
        "network.mobile.reset-day",
        "network.mobile.limit",
        "settings.network.mobile.used.reset",
    ] {
        assert!(
            ui::widget::find_test_id(app.harness().root_widget(), id).is_some(),
            "{id} on the modem's page"
        );
    }
    assert!(
        edit_field(&mut app, kind, "reset_day", 29).is_err(),
        "only a day every month has"
    );
    edit_field(&mut app, kind, "reset_day", 3).unwrap();
    edit_field(&mut app, kind, "limit_gb", 10).unwrap();
    assert_eq!(app.settings::<NetworkUsage>().get().reset_day.get(), 3);
    let reported = app
        .with_widget::<crate::window::Inspected, _>(|settings| {
            area(settings).unwrap().model().borrow().inspection()
        })
        .unwrap();
    assert_eq!(reported["data_period_starts_on_day"], 3);
    assert_eq!(reported["data_limit_bytes"], 10_000_000_000_u64);
    invoke_section(&mut app, kind, "reset-data-used", serde_json::json!({})).unwrap();
    assert_eq!(app.settings::<NetworkUsage>().get().mobile_used(today), 0);
}

/// A modem service whose changes are followed by hand.
struct FollowedModem {
    modem: ScriptedModem,
    reads: std::sync::atomic::AtomicUsize,
    follower: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl MobileService for FollowedModem {
    fn snapshot(&self) -> MobileSnapshot {
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.modem.snapshot()
    }

    fn change(&self, change: &MobileChange) -> Result<(), ModemError> {
        self.modem.change(change)
    }

    fn follow(&self, changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any>> {
        *self.follower.lock().unwrap() = Some(Arc::from(changed));
        Some(Box::new(()))
    }
}

/// The open section reads the modems again when the modem service says
/// they changed, and only then: no timer reads them while nothing changes,
/// and a closed section reads nothing.
#[test]
fn the_shown_section_reads_the_modems_again_when_they_change() {
    let device = Arc::new(FollowedModem {
        modem: ScriptedModem::new(Some(MobileSample::Registered)),
        reads: std::sync::atomic::AtomicUsize::new(0),
        follower: Mutex::new(None),
    });
    let mut model = MobileModel::loaded_from(
        Reach::of(&cx()),
        Arc::clone(&device) as Arc<dyn MobileService>,
    );
    let reads = || device.reads.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(reads(), 1);
    let poll_until_read = |model: &mut MobileModel, count: usize| {
        let deadline = tilo_test_env::hang_deadline();
        while reads() < count || model.snapshot.is_some() {
            model.poll();
            assert!(
                std::time::Instant::now() < deadline,
                "the modems were never read again"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };

    let following = model.follow();
    let changed = device
        .follower
        .lock()
        .unwrap()
        .clone()
        .expect("the modems are followed while the section shows");
    poll_until_read(&mut model, 2);
    model.poll();
    assert_eq!(reads(), 2, "showing the section reads the modems once");

    changed();
    poll_until_read(&mut model, 3);
    model.poll();
    assert_eq!(reads(), 3, "one change, one read");

    drop(following);
    changed();
    model.poll();
    assert!(model.snapshot.is_none(), "a closed section reads nothing");
    assert_eq!(reads(), 3);
}
