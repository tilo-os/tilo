use super::*;

const PIN: &str = "1234";
const PUK: &str = "12345678";

fn cx() -> PaneCx {
    let profile = tempfile::tempdir().unwrap().keep();
    PaneCx::in_file(profile.join("settings.json"))
}

fn scripted(sample: Option<MobileSample>) -> (Arc<ScriptedModem>, MobileModel) {
    let device = Arc::new(ScriptedModem::new(sample));
    let model = MobileModel::loaded_from(cx(), Arc::clone(&device) as Arc<dyn MobileService>);
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

fn said(model: &MobileModel) -> Vec<String> {
    crate::widgets::announced_text(&*model.compose())
}

fn says(model: &MobileModel, text: &str) -> bool {
    said(model).iter().any(|said| said.contains(text))
}

#[test]
fn without_a_modem_there_is_no_section_and_no_mobile_setting_is_tried() {
    let (device, mut model) = scripted(None);
    assert!(model.loaded);
    assert!(!model.present());
    assert!(model.sections().is_empty(), "nothing is shown");
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
    let radios = app.surface_mut::<SettingsApp>().host().radios().clone();
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
    cx.radios().set(Present::NONE);
    assert!(!unread.statuses().is_empty(), "the kernel lists no modem");
    cx.radios().set(Present::ALL);
    assert!(
        unread.statuses().is_empty(),
        "a modem the kernel lists is offered before it is read"
    );

    radios.set(Present::ALL);
    // Once read, ModemManager's answer is the one: the scripted device
    // has no modem, whatever the kernel says.
    app.with_surface::<SettingsApp, _>(|settings| {
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
    app.with_surface::<SettingsApp, _>(|settings| {
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
    app.with_surface::<SettingsApp, _>(|settings| {
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
    let (_, model) = scripted(Some(MobileSample::Registered));
    assert!(model.present());
    assert!(model.data_on());
    let said = said(&model);
    for expected in [
        "Telekom.de",
        "LTE, good signal, connected",
        "Data roaming",
        "Data used",
    ] {
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
    assert!(says(&model, "Orange F"));
    assert!(says(&model, "roaming"));
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

#[test]
fn a_locked_sim_asks_for_its_pin_counts_wrong_tries_and_unlocks() {
    let (_, mut model) = scripted(Some(MobileSample::Locked));
    assert!(says(&model, "SIM locked"));
    assert!(says(&model, "2 tries left."));
    assert!(
        !says(&model, "Data roaming"),
        "nothing else until it is unlocked"
    );

    *model.pin.lock().unwrap() = "0000".into();
    let wrong = run(
        &mut model,
        MobileEffect::UnlockSim {
            sim: None,
            pin: None,
        },
    );
    assert_eq!(wrong.unwrap_err(), "Wrong PIN");
    assert!(model.pin.lock().unwrap().is_empty(), "the code is not kept");
    assert!(says(&model, "One try left."));
    assert!(says(&model, "Wrong PIN"));

    *model.pin.lock().unwrap() = PIN.into();
    run(
        &mut model,
        MobileEffect::UnlockSim {
            sim: None,
            pin: None,
        },
    )
    .unwrap();
    assert_eq!(model.modem().unwrap().lock, SimLock::None);
    assert!(says(&model, "SIM unlocked."));
    assert!(!says(&model, "Wrong PIN"));
    assert!(says(&model, "Data roaming"));
}

#[test]
fn the_last_wrong_pin_blocks_the_sim_and_the_puk_unblocks_it() {
    let (_, mut model) = scripted(Some(MobileSample::Locked));
    for _ in 0..2 {
        let _ = run(
            &mut model,
            MobileEffect::UnlockSim {
                sim: None,
                pin: Some("9999".into()),
            },
        );
    }
    assert_eq!(model.modem().unwrap().lock, SimLock::Puk);
    assert!(says(&model, "SIM blocked"));
    assert!(says(&model, "PUK"));
    assert!(
        !model.modems.iter().any(|modem| modem.lock == SimLock::Pin),
        "the PIN alone no longer unlocks"
    );

    let wrong = run(
        &mut model,
        MobileEffect::UnblockSim {
            sim: None,
            puk: Some("00000000".into()),
            pin: Some("4321".into()),
        },
    );
    assert_eq!(wrong.unwrap_err(), "Wrong PUK");
    *model.puk.lock().unwrap() = PUK.into();
    *model.new_pin.lock().unwrap() = "4321".into();
    run(
        &mut model,
        MobileEffect::UnblockSim {
            sim: None,
            puk: None,
            pin: None,
        },
    )
    .unwrap();
    assert_eq!(model.modem().unwrap().lock, SimLock::None);
    assert!(model.puk.lock().unwrap().is_empty());
    assert!(model.new_pin.lock().unwrap().is_empty());
}

#[test]
fn the_puk_sample_starts_blocked() {
    let (_, model) = scripted(Some(MobileSample::Puk));
    assert_eq!(model.modem().unwrap().lock, SimLock::Puk);
    assert!(says(&model, "SIM blocked"));
    assert!(says(&model, "New PIN"));
}

#[test]
fn turning_the_pin_lock_off_asks_for_the_pin_first() {
    let (device, mut model) = scripted(Some(MobileSample::Registered));
    run(
        &mut model,
        MobileEffect::SetSimPinLock {
            sim: None,
            on: false,
            pin: None,
        },
    )
    .unwrap();
    assert_eq!(model.sim_form, Some(SimForm::PinLock(false)));
    assert!(
        device.changes().is_empty(),
        "nothing is sent without the PIN"
    );
    assert!(says(&model, "Enter the SIM's PIN to stop asking for it."));

    *model.pin.lock().unwrap() = PIN.into();
    run(
        &mut model,
        MobileEffect::SetSimPinLock {
            sim: None,
            on: false,
            pin: None,
        },
    )
    .unwrap();
    assert_eq!(model.sim_form, None);
    assert_eq!(model.modem().unwrap().pin_enabled, Some(false));
}

#[test]
fn changing_the_pin_takes_the_current_one() {
    let (_, mut model) = scripted(Some(MobileSample::Registered));
    run(&mut model, MobileEffect::ShowSimPinChange).unwrap();
    assert!(says(&model, "Current PIN"));
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
    assert!(says(&model, "PIN changed."));
}

#[test]
fn codes_never_reach_the_inspection_the_log_or_the_screen() {
    let (device, mut model) = scripted(Some(MobileSample::Locked));
    *model.pin.lock().unwrap() = "8642".into();
    let _ = run(
        &mut model,
        MobileEffect::UnlockSim {
            sim: None,
            pin: None,
        },
    );
    *model.pin.lock().unwrap() = "97531".into();
    assert!(!model.inspection().to_string().contains("97531"));
    assert!(!said(&model).iter().any(|text| text.contains("97531")));
    assert!(
        !device
            .changes()
            .iter()
            .any(|change| change.contains("8642"))
    );
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
    assert!(says(&model, "nmcli is not installed"));
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

/// 2026-03-14 12:00 UTC, in seconds since the epoch.
const MARCH_14: i64 = 1_773_489_600;
const DAY: i64 = 86_400;

#[test]
fn data_used_counts_traffic_deltas_through_restarts_and_starts_again_on_its_day() {
    let mut usage = MobileDataUsage {
        mobile_reset_day: 15,
        ..MobileDataUsage::default()
    };
    assert!(usage.account(Some(1_000), Some("a"), MARCH_14));
    assert_eq!(
        usage.mobile_used, 0,
        "the first total only starts the count"
    );
    assert_eq!(
        usage.period_start(),
        (2, 15),
        "March 14 is in the period from February 15"
    );

    usage.account(Some(1_500), Some("a"), MARCH_14 + 60);
    assert_eq!(usage.mobile_used, 500);
    assert!(
        !usage.account(Some(1_500), Some("a"), MARCH_14 + 120),
        "nothing new"
    );

    usage.account(Some(200), Some("a"), MARCH_14 + 180);
    assert_eq!(
        usage.mobile_used, 700,
        "a counter that started again counts from zero"
    );

    usage.account(Some(300), Some("b"), MARCH_14 + 240);
    assert_eq!(usage.mobile_used, 1_000, "another boot counts from zero");

    usage.account(Some(350), Some("b"), MARCH_14 + DAY);
    assert_eq!(
        usage.mobile_used, 50,
        "the period started again on the 15th"
    );
    assert_eq!(usage.period_start(), (3, 15));

    usage.account(None, None, MARCH_14 + 2 * DAY);
    assert_eq!(usage.mobile_used, 50, "no traffic read changes nothing");
}

#[test]
fn the_reset_day_and_reset_keep_the_count_on_this_device() {
    let (_, mut model) = scripted(Some(MobileSample::Registered));
    run(&mut model, MobileEffect::SetDataResetDay { day: 29 }).unwrap_err();
    run(&mut model, MobileEffect::SetDataResetDay { day: 3 }).unwrap();
    assert_eq!(
        model
            .cx
            .settings::<MobileDataUsage>()
            .get()
            .mobile_reset_day,
        3
    );
    model.usage.mobile_used = 42;
    run(&mut model, MobileEffect::ResetDataUsage).unwrap();
    assert_eq!(model.cx.settings::<MobileDataUsage>().get().mobile_used, 0);
}

#[test]
fn civil_dates_match_the_calendar() {
    assert_eq!(civil(0), (1970, 1, 1));
    assert_eq!(civil(MARCH_14.div_euclid(DAY)), (2026, 3, 14));
    assert_eq!(civil(11_016), (2000, 2, 29));
}
