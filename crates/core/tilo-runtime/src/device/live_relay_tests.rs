use super::*;
use tilo_user_identity::DeviceCapabilities;

fn cert(device: [u8; 32], expires_at_ns: Option<u64>) -> DeviceCert {
    DeviceCert {
        user_public_key: [1; 32],
        device_public_key: device,
        device_label: format!("device {}", device[0]),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns,
        signature: [0; 64],
        issuer: None,
    }
}

fn device(device: [u8; 32], expires_at_ns: Option<u64>) -> Device {
    Device {
        device,
        cert: cert(device, expires_at_ns),
    }
}

fn scope_of(workspace: WorkspaceId) -> Scope {
    Scope::workspace(workspace.to_string())
}

fn grant(workspace: WorkspaceId, device: &DeviceCert) -> grant::Native {
    grant::Native::new(
        *workspace.as_uuid().as_bytes(),
        &scope_of(workspace).id(),
        1,
        serde_json::to_vec(device).unwrap(),
        Vec::new(),
    )
    .unwrap()
}

/// Whether a relay that relays the workspace admits `grant` from the endpoint `peer` at `now_ns`.
fn admits(peer: [u8; 32], grant: &grant::Native, devices: &[Device], now_ns: u64) -> bool {
    let workspace = WorkspaceId::parse(&live::workspace_id(grant.workspace)).unwrap();
    bind(
        Peer(peer),
        grant,
        &scope_of(workspace),
        devices,
        true,
        now_ns,
    )
    .is_ok()
}

tilo_test_env::scenario! {
    fn the_daemons_relay_admits_the_devices_its_certificates_and_grants_name;

    phase a_device_that_relays_nothing_turns_every_grant_away;
    {
        let workspace = WorkspaceId::new();
        let other = cert([2; 32], None);
        let devices = vec![device([1; 32], None), Device { device: [2; 32], cert: other.clone() }];
        let scope = scope_of(workspace);
        assert!(bind(Peer([2; 32]), &grant(workspace, &other), &scope, &devices, true, 10).is_ok());
        assert_eq!(
            bind(Peer([2; 32]), &grant(workspace, &other), &scope, &devices, false, 10)
                .err()
                .map(|why| why.word()),
            Some("not-relayed-here")
        );
    }

    phase a_grant_is_admitted_only_for_the_handshakes_own_device;
    {
        let workspace = WorkspaceId::new();
        let mine = cert([1; 32], None);
        let other = cert([2; 32], None);
        let devices = vec![
            Device { device: [1; 32], cert: mine.clone() },
            Device { device: [2; 32], cert: other.clone() },
        ];
        assert!(admits([1; 32], &grant(workspace, &mine), &devices, 10));
        assert!(
            !admits([2; 32], &grant(workspace, &mine), &devices, 10),
            "a device presenting another's certificate is refused"
        );
        assert!(
            !admits([3; 32], &grant(workspace, &other), &devices, 10),
            "a device that is not on the roster is refused"
        );
    }

    phase a_delegated_transport_is_admitted_as_its_device_only_while_it_lasts;
    // A page keeps its device key unextractable, so it dials under a key of its own and presents
    // the device's signature for it: the relay admits that endpoint as the device, and refuses an
    // endpoint the device never signed for or whose delegation ran out.
    {
        let workspace = WorkspaceId::new();
        let device_key = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let page_key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let page = page_key.verifying_key().to_bytes();
        let mine = cert(device_key.verifying_key().to_bytes(), None);
        let devices = vec![Device { device: mine.device_public_key, cert: mine.clone() }];
        let delegation = tilo_user_identity::sign_endpoint_delegation(
            &device_key,
            page,
            100,
            60 * 60 * 1_000_000_000,
        )
        .unwrap();
        let mut delegated = grant(workspace, &mine);
        delegated.delegation = serde_json::to_vec(&delegation).unwrap();
        assert!(admits(page, &delegated, &devices, 200));
        assert!(
            !admits(page, &grant(workspace, &mine), &devices, 200),
            "without the device's signature the endpoint is somebody else"
        );
        let later = 100 + 60 * 60 * 1_000_000_000;
        assert!(!admits(page, &delegated, &devices, later), "an expired delegation admits nobody");
        let mut anyone = delegated;
        anyone.delegation = serde_json::to_vec(
            &tilo_user_identity::sign_endpoint_delegation(&page_key, page, 100, 1000).unwrap(),
        )
        .unwrap();
        assert!(!admits(page, &anyone, &devices, 200), "an endpoint cannot vouch for itself");
    }

    phase a_relay_that_turns_a_device_away_says_why;
    // Each case by its own word: a device that is not in the workspace, one presenting a
    // certificate the workspace does not hold for it, an endpoint that is not the device, a scope
    // this relay does not carry, and an expired certificate.
    {
        let workspace = WorkspaceId::new();
        let other = cert([2; 32], None);
        let devices = vec![device([1; 32], None), Device { device: [2; 32], cert: other.clone() }];
        let why = |peer: [u8; 32], grant: &grant::Native| {
            bind(Peer(peer), grant, &scope_of(workspace), &devices, true, 10)
                .err()
                .map(|refusal| refusal.word())
        };
        assert_eq!(why([2; 32], &grant(workspace, &other)), None);
        assert_eq!(why([3; 32], &grant(workspace, &cert([3; 32], None))), Some("not-a-member"));
        let mut renamed = other.clone();
        renamed.device_label = "Another name".to_owned();
        assert_eq!(why([2; 32], &grant(workspace, &renamed)), Some("certificate-differs"));
        assert_eq!(why([3; 32], &grant(workspace, &other)), Some("endpoint-not-the-device"));
        let mut elsewhere = grant(workspace, &other);
        elsewhere.scope = Scope::resource(workspace.to_string(), "elsewhere").id();
        assert_eq!(why([2; 32], &elsewhere), Some("scope-not-carried"));
        let expired = cert([2; 32], Some(5));
        assert_eq!(why([2; 32], &grant(workspace, &expired)), Some("certificate-expired"));
    }

    phase a_devices_own_scope_is_bound_like_a_workspaces_and_to_its_device;
    // The person's other device dials this one's own scope with the same grant, naming that
    // scope: it is bound to the handshake and the roster as a workspace's is, and a grant for one
    // device's scope is no grant for another's or for the workspace's.
    {
        let workspace = WorkspaceId::new();
        let other = cert([2; 32], None);
        let devices = vec![device([1; 32], None), Device { device: [2; 32], cert: other.clone() }];
        let here = Scope::device(workspace.to_string(), [1; 32]);
        let mut driving = grant(workspace, &other);
        driving.scope = here.id();
        let bound = bind(Peer([2; 32]), &driving, &here, &devices, true, 10).ok().unwrap();
        assert_eq!((bound.scope, bound.device), (here.clone(), [2; 32]));
        let why = |scope: &Scope, grant: &grant::Native| {
            bind(Peer([2; 32]), grant, scope, &devices, true, 10).err().map(|refusal| refusal.word())
        };
        let beside = Scope::device(workspace.to_string(), [3; 32]);
        assert_eq!(why(&beside, &driving), Some("scope-not-carried"));
        assert_eq!(why(&scope_of(workspace), &driving), Some("scope-not-carried"));
        assert_eq!(why(&here, &grant(workspace, &other)), Some("scope-not-carried"));
        let elsewhere = Scope::device(WorkspaceId::new().to_string(), [1; 32]);
        let mut astray = grant(workspace, &other);
        astray.scope = elsewhere.id();
        assert_eq!(why(&elsewhere, &astray), Some("scope-not-carried"));
    }

    phase a_device_behind_the_relays_keys_is_turned_away_with_both_epochs;
    {
        assert_eq!(keys_current(3, 4), Err(Refusal::KeysBehind { presented: 3, newest: 4 }));
        assert_eq!(keys_current(4, 4), Ok(()));
        assert_eq!(keys_current(5, 4), Ok(()));
    }

    phase what_a_relay_keeps_of_the_devices_it_turned_away_is_bounded;
    // Bounded, counts how often each asked, and says only a new reason as news.
    {
        let mut turned = BTreeMap::new();
        let behind = Refusal::KeysBehind { presented: 3, newest: 4 };
        assert!(note_turned_away(&mut turned, [1; 32], "Ada's laptop".to_owned(), behind));
        assert!(!note_turned_away(
            &mut turned,
            [1; 32],
            "Ada's laptop".to_owned(),
            Refusal::KeysBehind { presented: 3, newest: 5 }
        ));
        assert!(note_turned_away(&mut turned, [1; 32], "Ada's laptop".to_owned(), Refusal::NotAMember));
        assert_eq!(turned.get(&[1; 32]).map(|turned| turned.times), Some(3));
        for peer in 2..=20_u8 {
            note_turned_away(&mut turned, [peer; 32], String::new(), Refusal::NotAMember);
        }
        assert_eq!(turned.len(), TURNED_AWAY_KEPT);
    }

    phase the_reading_says_each_device_turned_away_and_why;
    // What the person's shell shows of a device turned away: its key, its name, why in the word a
    // tool branches on, how often it asked, and for keys behind the epoch it presented and the
    // relay's.
    {
        let behind = TurnedAway {
            peer: [0x0f; 32],
            name: "Ada's laptop".to_owned(),
            why: Refusal::KeysBehind { presented: 3, newest: 4 },
            times: 7,
        };
        assert_eq!(
            behind.row(),
            serde_json::json!({
                "device": hex::encode([0x0f; 32]),
                "name": "Ada's laptop",
                "why": "keys-behind",
                "presented": 3,
                "newest": 4,
                "times": 7,
            })
        );
        let stranger = TurnedAway {
            peer: [0x19; 32],
            name: String::new(),
            why: Refusal::NotAMember,
            times: 1,
        };
        assert_eq!(
            stranger.row(),
            serde_json::json!({
                "device": hex::encode([0x19; 32]),
                "name": "",
                "why": "not-a-member",
                "presented": null,
                "newest": null,
                "times": 1,
            })
        );
    }

    phase a_device_is_named_as_its_certificate_names_it;
    {
        let devices = vec![device([1; 32], None), device([2; 32], None)];
        assert_eq!(name_of(&devices, [2; 32]), "device 2");
        assert_eq!(name_of(&devices, [7; 32]), "");
    }
}

/// What a session of the workspace called `name` is offered, as `device` and as a browser
/// environment's device when `environment`.
fn tracks(name: &str, device: [u8; 32], environment: bool) -> Tracks {
    let scope = Scope::workspace(name);
    Tracks {
        workspace: name.to_owned(),
        scope: scope.id(),
        publication: Namespace::new(&scope, device).path(),
        prefix: live::devices(&scope),
        device,
        to: None,
        environment,
    }
}

/// One device's namespace in the workspace called `name`.
fn names(name: &str, device: [u8; 32]) -> Namespace {
    Namespace::new(&Scope::workspace(name), device)
}

tilo_test_env::scenario! {
    fn a_session_is_offered_its_own_tracks_and_the_scope_it_hears;

    phase a_member_hears_every_device_of_its_workspace;
    {
        let offered = tracks("ws-1", [1; 32], false);
        assert_eq!(offered.heard(), Some(offered.prefix.as_str()));
        let offered_to = |direction, name: &Namespace| offered.current(direction, name, |_| true);
        assert!(offered_to(Direction::Subscribe, &names("ws-1", [2; 32])));
        assert!(offered_to(Direction::Publish, &names("ws-1", [1; 32])));
        assert!(
            !offered_to(Direction::Publish, &names("ws-1", [2; 32])),
            "nobody publishes in another device's name"
        );
    }

    phase a_browser_environment_publishes_its_window_and_hears_nobody;
    {
        let offered = tracks("ws-1", [1; 32], true);
        assert_eq!(
            offered.publication(),
            names("ws-1", [1; 32]).path(),
            "it publishes its own window's page"
        );
        assert_eq!(
            offered.heard(),
            None,
            "no track of the workspace is offered to it: no call, camera, screen or other window"
        );
        let offered_to = |direction, name: &Namespace| offered.current(direction, name, |_| true);
        assert!(!offered_to(Direction::Subscribe, &names("ws-1", [2; 32])));
        assert!(!offered_to(Direction::Subscribe, &names("ws-1", [1; 32])));
        assert!(offered_to(Direction::Publish, &names("ws-1", [1; 32])));
    }
}

tilo_test_env::scenario! {
    fn a_relay_that_comes_back_is_asked_again_soon_and_the_devices_ask_it_one_after_another;

    phase a_relay_that_was_missed_is_asked_again_ever_less_often_and_always;
    // A workspace whose relay did not take this device in asks it again within a second, then
    // ever less often, and never stops asking: the relay it waits for may come back at any time,
    // and says nothing when it does.
    {
        assert_eq!(retry_schedule(1), RETRY_FIRST);
        assert_eq!(retry_schedule(2), RETRY_FIRST * 2);
        assert_eq!(retry_schedule(3), RETRY_FIRST * 4);
        let waits: Vec<Duration> = (1..40).map(retry_schedule).collect();
        assert!(waits.windows(2).all(|pair| pair[0] <= pair[1]), "{waits:?}");
        assert_eq!(waits.last(), Some(&RETRY_LONGEST));
        assert_eq!(retry_schedule(u32::MAX), RETRY_LONGEST);
        assert!(RETRY_LONGEST < REREAD, "a relay that is away is asked more often than the reread");
        let first_steps: Duration = (1..=4).map(retry_schedule).sum();
        assert!(
            first_steps <= Duration::from_secs(15),
            "a relay back within a few seconds is rejoined within the first steps: {first_steps:?}"
        );
    }

    phase devices_that_missed_a_relay_together_do_not_ask_it_again_together;
    // The devices of a workspace that missed its relay in one moment do not ask it again in one
    // moment: each takes up to a quarter off the schedule's wait, by its own key, and none waits
    // longer than the schedule.
    {
        let workspace = WorkspaceId::new();
        for missed in 1..=6 {
            let schedule = retry_schedule(missed);
            let waits: BTreeSet<Duration> = (0..64_u8)
                .map(|device| retry_wait([device; 32], workspace, missed))
                .collect();
            assert!(
                waits
                    .iter()
                    .all(|wait| *wait <= schedule && *wait >= schedule.saturating_sub(schedule / 4)),
                "after {missed} misses every device waits within a quarter of {schedule:?}: {waits:?}"
            );
            assert!(
                waits.len() > 56,
                "sixty-four devices wait {} different times after {missed} misses",
                waits.len()
            );
        }
    }
}
