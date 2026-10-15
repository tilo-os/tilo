use super::*;
use crate::resolved_invocation_tests::over_wire;

use capnp::capability::Promise;
use std::cell::{Cell, RefCell};

const HOST: [u8; 16] = [2; 16];
const ID: [u8; 16] = [1; 16];
const UPDATE: &str = r#"{"state":"accepted","invocation":"01010101-0101-0101-0101-010101010101"}"#;
fn binding() -> PublishedInvocationBinding {
    PublishedInvocationBinding {
        host: HOST,
        peer_id: 9,
        app_id: "notes".into(),
        record_id: String::new(),
        root: [3; 32],
        device: [4; 32],
        target_binding: [5; 32],
    }
}
fn write_binding(mut b: collab_capnp::published_invocation_binding::Builder<'_>) {
    b.set_host(&HOST);
    b.set_peer_id(9);
    b.set_app_id("notes");
    b.set_record_id("");
    b.set_root(&[3; 32]);
    b.set_device(&[4; 32]);
    b.set_target_binding(&[5; 32]);
}
/// A complete exact row for `r`/`a`, as the broker writes one: the
/// decoder refuses a descriptor without its operation kind and contract.
fn row(r: collab_capnp::action_directory_row::Builder<'_>) {
    let contract = tilo_actions::ActionContract::view();
    let descriptor = ExportedAction {
        resource: "r".into(),
        semantic_identity: None,
        action: "a".into(),
        resource_kind: "note".into(),
        resource_label: "Note".into(),
        action_label: "Open".into(),
        description: None,
        operation_kind: tilo_actions::OperationKind::View,
        params: Vec::new(),
        contract: contract.clone(),
        view: None,
        input: ExportedInput::None,
    };
    write_action_directory_row(
        r,
        &ActionDirectoryRow {
            parent_resource: String::new(),
            resource: "r".into(),
            resource_kind: "note".into(),
            resource_label: "Note".into(),
            presentation: None,
            action: Some("a".into()),
            descriptor: Some(descriptor),
            has_children: false,
            visibility: Some(RowVisibility {
                governing_kind: Some(tilo_actions::OperationKind::View),
                action: "a".into(),
                contract,
            }),
        },
    )
    .unwrap();
}
fn observation(mut o: collab_capnp::published_invocation_observation::Builder<'_>, mode: u8) {
    o.set_host(&HOST);
    o.set_invocation(&ID);
    o.set_found(true);
    o.set_id(99);
    o.set_update_json(UPDATE);
    match mode {
        1 => o.set_host(&[8;16]),
        2 => o.set_invocation(&[8;16]),
        3 => o.set_host(&[2;15]),
        4 => o.set_invocation(&[1;17]),
        5 => o.set_update_json(r#"{"state":"accepted","invocation":"08080808-0808-0808-0808-080808080808"}"#),
        6 => o.set_update_json("not json"),
        7 => o.set_update_json("{}"),
        8 => o.set_found(false),
        9 => {o.set_found(false);o.set_id(0);o.set_update_json("");o.set_uncertain(true);},
        10 => {o.set_found(false);o.set_id(0);o.set_update_json("");o.set_cancellation_requested(true);},
        11 => o.set_update_json("x".repeat(256*1024+1)),
        12 => o.set_update_json(""),
        13 => o.set_update_json(r#"{"invocation":"01010101-0101-0101-0101-010101010101","invocation":"01010101-0101-0101-0101-010101010101"}"#),
        14 => {o.set_found(false);o.set_id(0);o.set_update_json("");},
        15 => {o.set_uncertain(true);o.set_cancellation_requested(true);},
        16 => o.set_host(&[0;16]),
        17 => o.set_invocation(&[0;16]),
        18 => o.init_update_json(1).as_bytes_mut()[0]=0xff,
        19 => o.set_update_json(r#"{"invocation":"01010101010101010101010101010101"}"#),
        20 => o.set_update_json(format!(r#"{{"state":"accepted","invocation":"01010101-0101-0101-0101-010101010101","padding":"{}"}}"#,"x".repeat(240*1024))),
        21 => o.set_id(0),
        _ => {}
    }
}
type Received = Rc<RefCell<Vec<(String, String, String, Option<String>, String)>>>;
struct Server {
    calls: Rc<Cell<usize>>,
    mode: u8,
    received: Received,
}
impl collab_capnp::channel_handle::Server for Server {
    fn resolve_published_action(
        self: Rc<Self>,
        params: collab_capnp::channel_handle::ResolvePublishedActionParams,
        mut results: collab_capnp::channel_handle::ResolvePublishedActionResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        Promise::from((|| {
            self.calls.set(self.calls.get().saturating_add(1));
            let p = params.get()?;
            assert_eq!(p.get_peer_id(), 9);
            row(results.get().init_resolution().init_found());
            if self.mode != 1 {
                write_binding(results.get().init_invocation_binding());
            }
            match self.mode {
                2 => results.get().get_invocation_binding()?.set_host(&[2; 15]),
                3 => results.get().get_invocation_binding()?.set_peer_id(10),
                4 => results
                    .get()
                    .get_invocation_binding()?
                    .set_app_id("x".repeat(513)),
                5 => results.get().get_invocation_binding()?.set_root(&[0; 32]),
                6 => results.get().init_resolution().set_not_found(()),
                7 => results
                    .get()
                    .get_invocation_binding()?
                    .set_target_binding(&[0; 32]),
                8 => results
                    .get()
                    .init_resolution()
                    .init_found()
                    .set_resource("other"),
                9 => results
                    .get()
                    .init_resolution()
                    .init_found()
                    .set_resource_label("x".repeat(256 * 1024)),
                10 => results.get().get_invocation_binding()?.set_root(&[3; 31]),
                11 => results.get().get_invocation_binding()?.set_device(&[4; 33]),
                12 => results
                    .get()
                    .get_invocation_binding()?
                    .set_target_binding(&[5; 31]),
                13 => results
                    .get()
                    .get_invocation_binding()?
                    .set_app_id("notes\n"),
                14 => results
                    .get()
                    .get_invocation_binding()?
                    .set_record_id("r".repeat(513)),
                15 => results.get().get_invocation_binding()?.set_host(&[0; 16]),
                _ => {}
            }
            Ok(())
        })())
    }
    fn invoke_resolved_published_action_durable(
        self: Rc<Self>,
        params: collab_capnp::channel_handle::InvokeResolvedPublishedActionDurableParams,
        mut results: collab_capnp::channel_handle::InvokeResolvedPublishedActionDurableResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        Promise::from((|| {
            self.calls.set(self.calls.get().saturating_add(1));
            let p = params.get()?;
            assert_eq!(p.get_invocation()?, ID);
            let b = p.get_binding()?;
            assert_eq!(b.get_host()?, HOST);
            assert_eq!(b.get_peer_id(), 9);
            assert_eq!(b.get_app_id()?.to_str()?, "notes");
            assert_eq!(b.get_record_id()?.to_str()?, "");
            assert_eq!(b.get_root()?, [3; 32]);
            assert_eq!(b.get_device()?, [4; 32]);
            assert_eq!(b.get_target_binding()?, [5; 32]);
            self.received.borrow_mut().push((
                p.get_revision()?.to_str()?.into(),
                p.get_resource()?.to_str()?.into(),
                p.get_action()?.to_str()?.into(),
                if p.get_has_argument() {
                    Some(p.get_argument()?.to_str()?.into())
                } else {
                    None
                },
                p.get_confirmation_json()?.to_str()?.into(),
            ));
            observation(results.get().init_observation(), self.mode);
            Ok(())
        })())
    }
    fn published_invocation_observation(
        self: Rc<Self>,
        params: collab_capnp::channel_handle::PublishedInvocationObservationParams,
        mut results: collab_capnp::channel_handle::PublishedInvocationObservationResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        Promise::from((|| {
            self.calls.set(self.calls.get().saturating_add(1));
            let p = params.get()?;
            assert_eq!(p.get_host()?, HOST);
            assert_eq!(p.get_invocation()?, ID);
            observation(results.get().init_observation(), self.mode);
            Ok(())
        })())
    }
    fn cancel_durable_published_invocation(
        self: Rc<Self>,
        params: collab_capnp::channel_handle::CancelDurablePublishedInvocationParams,
        mut results: collab_capnp::channel_handle::CancelDurablePublishedInvocationResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        Promise::from((|| {
            self.calls.set(self.calls.get().saturating_add(1));
            let p = params.get()?;
            assert_eq!(p.get_host()?, HOST);
            assert_eq!(p.get_invocation()?, ID);
            observation(results.get().init_observation(), self.mode);
            Ok(())
        })())
    }
}
fn server(mode: u8) -> (Server, Rc<Cell<usize>>, Received) {
    let calls = Rc::new(Cell::new(0));
    let received = Rc::default();
    (
        Server {
            calls: calls.clone(),
            mode,
            received: Rc::clone(&received),
        },
        calls,
        received,
    )
}
#[tokio::test(flavor = "current_thread")]
async fn reliable_invocation_round_trip_preserves_business_identity_and_optional_argument() {
    let (s, calls, received) = server(0);
    over_wire(s, |c| async move {
        let (row, b) = c
            .resolve_published_action_for_invocation(9, "v", "r", "a")
            .await
            .unwrap();
        assert_eq!(*row.action.as_ref().unwrap(), "a");
        assert_eq!(b, binding());
        for a in [None, Some(""), Some("exact")] {
            let o = c
                .invoke_resolved_published_action_durable(&ID, &b, "v", "r", "a", a, "{}")
                .await
                .unwrap();
            assert_eq!(o.host, HOST);
            assert_eq!(o.invocation, ID);
            let r = o.result.unwrap();
            assert_eq!(r.id, 99);
            assert_eq!(r.update_json, UPDATE);
            assert!(!r.uncertain);
        }
        assert_eq!(
            c.published_invocation_observation(&HOST, &ID)
                .await
                .unwrap(),
            c.cancel_durable_published_invocation(&HOST, &ID)
                .await
                .unwrap()
        );
    })
    .await;
    assert_eq!(calls.get(), 6);
    assert_eq!(
        *received.borrow(),
        [None, Some(""), Some("exact")].map(|a| (
            "v".into(),
            "r".into(),
            "a".into(),
            a.map(str::to_owned),
            "{}".into()
        ))
    );
}
#[tokio::test(flavor = "current_thread")]
async fn reliable_invocation_old_35_without_binding_refuses_before_effect() {
    let (s, calls, _) = server(1);
    over_wire(s, |c| async move {
        assert!(matches!(
            c.resolve_published_action(9, "v", "r", "a").await.unwrap(),
            ActionDirectoryResolution::Found { .. }
        ));
        assert!(
            c.resolve_published_action_for_invocation(9, "v", "r", "a")
                .await
                .is_err()
        );
    })
    .await;
    assert_eq!(calls.get(), 2);
}
#[tokio::test(flavor = "current_thread")]
async fn reliable_invocation_rejects_malformed_or_replaced_binding_and_resolution() {
    for mode in 2..=15 {
        let (s, calls, _) = server(mode);
        over_wire(s, move |c| async move {
            assert!(
                c.resolve_published_action_for_invocation(9, "v", "r", "a")
                    .await
                    .is_err(),
                "mode {mode}"
            );
        })
        .await;
        assert_eq!(calls.get(), 1);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn reliable_invocation_rejects_uncorrelated_or_inconsistent_observation() {
    for mode in (1..=13).chain(16..=19) {
        let (s, calls, _) = server(mode);
        over_wire(s, move |c| async move {
            assert!(
                c.invoke_resolved_published_action_durable(
                    &ID,
                    &binding(),
                    "v",
                    "r",
                    "a",
                    None,
                    "{}"
                )
                .await
                .is_err(),
                "invoke mode {mode}"
            );
            assert!(
                c.published_invocation_observation(&HOST, &ID)
                    .await
                    .is_err(),
                "observe mode {mode}"
            );
            assert!(
                c.cancel_durable_published_invocation(&HOST, &ID)
                    .await
                    .is_err(),
                "cancel mode {mode}"
            );
        })
        .await;
        assert_eq!(calls.get(), 3);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn reliable_invocation_retains_absence_and_uncertainty_without_inventing_outcome() {
    for mode in [14, 15] {
        let (s, _, _) = server(mode);
        over_wire(s, move |c| async move {
            let o = c
                .published_invocation_observation(&HOST, &ID)
                .await
                .unwrap();
            if mode == 14 {
                assert!(o.result.is_none());
            } else {
                let r = o.result.unwrap();
                assert!(r.uncertain);
                assert!(r.cancellation_requested);
                assert_eq!(r.update_json, UPDATE);
            }
        })
        .await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn reliable_invocation_input_bounds_precede_every_request_copy() {
    let (s, calls, _) = server(0);
    over_wire(s, |c| async move {
        for (field, value) in [
            (0, String::new()),
            (0, "v".repeat(513)),
            (0, "v\0".into()),
            (1, String::new()),
            (1, "r\n".into()),
            (1, "r".repeat(513)),
            (2, "a\u{7f}".into()),
            (2, "a".repeat(513)),
            (3, "é".repeat(16385)),
            (3, "a\0".into()),
            (4, " ".repeat(4097)),
            (4, "{}\0".into()),
        ] {
            let mut a = ["v", "r", "a", "arg", "{}"];
            a[field] = &value;
            assert!(
                c.invoke_resolved_published_action_durable(
                    &ID,
                    &binding(),
                    a[0],
                    a[1],
                    a[2],
                    Some(a[3]),
                    a[4]
                )
                .await
                .is_err()
            );
            if field <= 2 {
                assert!(
                    c.resolve_published_action_for_invocation(9, a[0], a[1], a[2])
                        .await
                        .is_err()
                );
            }
        }
        for mode in 0..9 {
            let mut b = binding();
            match mode {
                0 => b.host = [0; 16],
                1 => b.peer_id = 0,
                2 => b.peer_id = u64::MAX,
                3 => b.app_id.clear(),
                4 => b.app_id = "x".repeat(513),
                5 => b.record_id = "x\n".into(),
                6 => b.root = [0; 32],
                7 => b.device = [0; 32],
                _ => b.target_binding = [0; 32],
            }
            assert!(
                c.invoke_resolved_published_action_durable(&ID, &b, "v", "r", "a", None, "{}")
                    .await
                    .is_err()
            );
        }
        assert!(
            c.invoke_resolved_published_action_durable(
                &[0; 16],
                &binding(),
                "v",
                "r",
                "a",
                None,
                "{}"
            )
            .await
            .is_err()
        );
        for peer in [0, u64::MAX] {
            assert!(
                c.resolve_published_action_for_invocation(peer, "v", "r", "a")
                    .await
                    .is_err()
            );
        }
        for (host, id) in [([0; 16], ID), (HOST, [0; 16])] {
            assert!(
                c.published_invocation_observation(&host, &id)
                    .await
                    .is_err()
            );
            assert!(
                c.cancel_durable_published_invocation(&host, &id)
                    .await
                    .is_err()
            );
        }
    })
    .await;
    assert_eq!(calls.get(), 0);
}
#[tokio::test(flavor = "current_thread")]
async fn reliable_invocation_accepts_inclusive_utf8_input_limits() {
    let (s, _, received) = server(0);
    over_wire(s, |c| async move {
        c.invoke_resolved_published_action_durable(
            &ID,
            &binding(),
            &"é".repeat(256),
            &"r".repeat(512),
            &"a".repeat(512),
            Some(&"é".repeat(16384)),
            &" ".repeat(4096),
        )
        .await
        .unwrap();
    })
    .await;
    assert_eq!(received.borrow()[0].3.as_ref().unwrap().len(), 32768);
}
#[tokio::test(flavor = "current_thread")]
async fn reliable_invocation_accepts_bounded_large_observation_without_correlation_loss() {
    let (s, _, _) = server(20);
    over_wire(s, |c| async move {
        let o = c
            .published_invocation_observation(&HOST, &ID)
            .await
            .unwrap();
        assert_eq!(o.invocation, ID);
        assert!(o.result.unwrap().update_json.len() > 240 * 1024);
    })
    .await;
}
async fn rejects_zero_wire_id(method: u8) {
    let (s, calls, _) = server(21);
    over_wire(s, move |c| async move {
        let result = match method {
            48 => {
                c.invoke_resolved_published_action_durable(
                    &ID,
                    &binding(),
                    "v",
                    "r",
                    "a",
                    None,
                    "{}",
                )
                .await
            }
            49 => c.published_invocation_observation(&HOST, &ID).await,
            50 => c.cancel_durable_published_invocation(&HOST, &ID).await,
            _ => unreachable!("test selects one actual reliable RPC"),
        };
        assert!(
            result.is_err(),
            "found=true id=0 was accepted by RPC @{method}"
        );
    })
    .await;
    assert_eq!(
        calls.get(),
        1,
        "actual malformed RPC reply must be observed"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn reliable_invocation_zero_wire_id_invoke_refuses() {
    rejects_zero_wire_id(48).await;
}
#[tokio::test(flavor = "current_thread")]
async fn reliable_invocation_zero_wire_id_observe_refuses() {
    rejects_zero_wire_id(49).await;
}
#[tokio::test(flavor = "current_thread")]
async fn reliable_invocation_zero_wire_id_cancel_refuses() {
    rejects_zero_wire_id(50).await;
}
struct Old47(Rc<Cell<usize>>);
impl collab_capnp::channel_handle::Server for Old47 {
    fn invoke_resolved_published_action(
        self: Rc<Self>,
        _: collab_capnp::channel_handle::InvokeResolvedPublishedActionParams,
        mut results: collab_capnp::channel_handle::InvokeResolvedPublishedActionResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        self.0.set(self.0.get().saturating_add(1));
        results.get().set_id(47);
        Promise::ok(())
    }
}
#[tokio::test(flavor = "current_thread")]
async fn reliable_invocation_old_47_server_has_no_fallback_effect() {
    let effects = Rc::new(Cell::new(0));
    let seen = effects.clone();
    over_wire(Old47(effects), |c| async move {
        let e = c
            .invoke_resolved_published_action_durable(&ID, &binding(), "v", "r", "a", None, "{}")
            .await
            .unwrap_err();
        assert!(matches!(e,NotAvailable::Capnp(e) if e.kind==capnp::ErrorKind::Unimplemented));
        assert_eq!(seen.get(), 0);
        assert_eq!(
            c.invoke_resolved_published_action(9, "v", "r", "a", None, "{}")
                .await
                .unwrap()
                .0,
            47
        );
        assert_eq!(seen.get(), 1);
    })
    .await;
}
#[test]
fn reliable_invocation_old_35_and_47_parameter_words_stay_exact() {
    let fixtures: [(bool, &[u64]); 2] = [
        (
            false,
            &[
                0x0000_0008_0000_0000,
                0x0003_0001_0000_0000,
                9,
                0x0000_0012_0000_0009,
                0x0000_0012_0000_0009,
                0x0000_0012_0000_0009,
                0x76,
                0x72,
                0x61,
            ],
        ),
        (
            true,
            &[
                0x0000_000d_0000_0000,
                0x0005_0002_0000_0000,
                9,
                1,
                0x0000_0012_0000_0011,
                0x0000_0012_0000_0011,
                0x0000_0012_0000_0011,
                0x0000_000a_0000_0011,
                0x0000_001a_0000_0011,
                0x76,
                0x72,
                0x61,
                0,
                0x7d7b,
            ],
        ),
    ];
    for (lazy, words) in fixtures {
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let decoded = capnp::serialize::read_message(
            &mut bytes.as_slice(),
            capnp::message::ReaderOptions::default(),
        )
        .unwrap();
        let mut out = capnp::message::Builder::new_default();
        if lazy {
            let p=decoded.get_root::<collab_capnp::channel_handle::invoke_resolved_published_action_params::Reader<'_>>().unwrap();
            assert!(p.get_has_argument());
            assert_eq!(p.get_revision().unwrap().to_str().unwrap(), "v");
            let mut p=out.init_root::<collab_capnp::channel_handle::invoke_resolved_published_action_params::Builder<'_>>();
            p.set_peer_id(9);
            p.set_has_argument(true);
            p.set_revision("v");
            p.set_resource("r");
            p.set_action("a");
            p.set_argument("");
            p.set_confirmation_json("{}");
        } else {
            let p=decoded.get_root::<collab_capnp::channel_handle::resolve_published_action_params::Reader<'_>>().unwrap();
            assert_eq!(p.get_revision().unwrap().to_str().unwrap(), "v");
            let mut p=out.init_root::<collab_capnp::channel_handle::resolve_published_action_params::Builder<'_>>();
            p.set_peer_id(9);
            p.set_revision("v");
            p.set_resource("r");
            p.set_action("a");
        }
        assert_eq!(capnp::serialize::write_message_to_words(&out), bytes);
    }
}
