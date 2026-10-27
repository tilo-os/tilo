use super::*;
use std::sync::{Arc, Mutex};
use tilo_actions::remote::*;
use tilo_desktop::activity::morning::MorningCommand;

struct Host {
    room: String,
    rows: Mutex<Vec<CatalogueRow>>,
    queries: Mutex<Vec<CatalogueQuery>>,
    commands: Mutex<Vec<InvokeRequest>>,
}
impl ActionHost for Host {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.queries.lock().unwrap().push(query.clone());
        if caller.room() != Some(self.room.as_str()) {
            return Pending::ready(Err(ActionHostError::NotPermitted));
        }
        let page = if query.max_results == 0 {
            CataloguePage {
                providers: vec![CatalogueProvider {
                    provider: "synthetic".into(),
                    app_id: "so.tilo.fixture".into(),
                    room: self.room.clone(),
                    record_id: String::new(),
                    browsable: true,
                }],
                ..Default::default()
            }
        } else {
            if query.target.is_none() {
                assert_eq!(query.provider.as_deref(), Some("synthetic"));
                assert_eq!(query.parent_resource.as_deref(), Some("fixture:chosen"));
                assert!(query.needle.is_empty());
                assert_eq!(query.max_results, 32);
            } else {
                assert_eq!(
                    query.target,
                    Some(tilo_actions::ActionAddress::new(
                        "fixture:chosen",
                        "refresh"
                    ))
                );
            }
            let rows = self.rows.lock().unwrap().clone();
            if query.target.is_none() && rows.len() <= 32 {
                CataloguePage::from_rows("synthetic-generation".into(), rows, &query)
            } else {
                // Exact resolution is independent of browsing. Deliberately
                // malformed oversized pages exercise the adapter's own bound.
                CataloguePage {
                    rows,
                    exhausted: true,
                    ..Default::default()
                }
            }
        };
        Pending::ready(Ok(page))
    }
    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        assert_eq!(caller.room(), Some(self.room.as_str()));
        let receipt = InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: tilo_actions::ActionOutput::default(),
            },
        };
        if request.target.address == tilo_actions::ActionAddress::new("fixture:chosen", "refresh") {
            for row in self.rows.lock().unwrap().iter_mut() {
                if row.resource.id == "fixture:chosen" {
                    row.resource.presentation.as_mut().unwrap().preview =
                        Some("Three project goals complete after the chosen refresh.".into());
                }
            }
        }
        self.commands.lock().unwrap().push(request);
        Pending::ready(Ok(receipt))
    }
    fn observe(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<InvocationState> {
        None
    }
    fn cancel(
        &self,
        _: &Caller,
        _: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        tilo_actions::remote::Pending::ready(Err(ActionHostError::UnknownInvocation))
    }
}
fn row() -> CatalogueRow {
    let descriptor: tilo_actions::ActionDescriptor = serde_json::from_value(serde_json::json!({
        "address": {"resource": "fixture:chosen", "action": "refresh"},
        "resource_kind": "fixture", "resource_label": "Chosen feed", "action_label": "Refresh",
        "locus": tilo_actions::ExecutionLocus::default(), "kind": tilo_actions::OperationKind::Effect,
        "contract": tilo_actions::ActionContract::view(), "input": tilo_actions::InputSpec::None, "params": []
    })).unwrap();
    CatalogueRow {
        provider: "synthetic".into(),
        app_id: "so.tilo.fixture".into(),
        parent_resource: "fixture:chosen".into(),
        resource: CatalogueResource {
            id: "fixture:chosen".into(),
            kind: "fixture".into(),
            label: "Chosen feed".into(),
            presentation: Some(tilo_actions::ResourcePresentation {
                preview: Some(
                    "Two project goals complete. The remaining review is pending.".into(),
                ),
                detail: Some("Synthetic project feed · observation 2026-10-11T08:00:00Z".into()),
                badge: Some("Last cached observation".into()),
            }),
        },
        action: Some(descriptor),
        has_children: false,
        visibility: None,
    }
}
fn fixture() -> (NativeMorningSources, Arc<Host>, MorningSource) {
    let host = Arc::new(Host {
        room: uuid::Uuid::new_v4().to_string(),
        rows: Mutex::new(vec![row()]),
        queries: Mutex::default(),
        commands: Mutex::default(),
    });
    let native = {
        let broker = crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
        NativeMorningSources {
            sessions: ActionSessionSource::with_mock(broker.clone(), host.clone()),
            broker,
            entries: RefCell::default(),
            commands: RefCell::default(),
        }
    };
    (
        native,
        host,
        MorningSource {
            app: "so.tilo.fixture".into(),
            resource: "fixture:chosen".into(),
        },
    )
}
#[test]
fn morning_payload_loads_the_chosen_resources_production_parent_scoped_action_page() {
    let (native, host, source) = fixture();
    assert!(native.payload(&host.room, &source).is_none());
    let loaded = native.payload(&host.room, &source);
    assert!(
        loaded.is_some(),
        "the chosen resource's published action page must provide its preview"
    );
    assert!(
        loaded
            .unwrap()
            .text()
            .contains("Two project goals complete")
    );
    assert!(host.commands.lock().unwrap().is_empty());
}

#[test]
fn morning_payload_queries_only_explicit_choice_and_loses_revoked_or_foreign_evidence() {
    let (native, host, source) = fixture();
    assert!(native.payload(&host.room, &source).is_none());
    let payload = native.payload(&host.room, &source).unwrap();
    assert!(payload.text().contains("Two project goals complete"));
    assert!(payload.refreshable);
    assert!(
        host.commands.lock().unwrap().is_empty(),
        "reading never invokes refresh"
    );
    assert_eq!(host.queries.lock().unwrap().len(), 2);
    host.rows.lock().unwrap().clear();
    native.entries.borrow_mut().clear();
    assert!(native.payload(&host.room, &source).is_none());
    assert!(
        native.payload(&host.room, &source).is_none(),
        "revoked/deleted row retains no cached payload"
    );
    assert!(
        native
            .payload(&uuid::Uuid::new_v4().to_string(), &source)
            .is_none()
    );
    assert!(
        native
            .entries
            .borrow()
            .values()
            .all(|entry| entry.payload.is_none())
    );
}
#[test]
fn morning_prompted_refresh_is_not_offered_or_invoked_without_its_input() {
    let (native, host, source) = fixture();
    host.rows.lock().unwrap()[0].action.as_mut().unwrap().input = tilo_actions::InputSpec::Text {
        prompt: "Choose refresh scope".into(),
        placeholder: String::new(),
        collects: tilo_actions::Collects::Words,
    };
    native.payload(&host.room, &source);
    let payload = native.payload(&host.room, &source).unwrap();
    assert!(!payload.refreshable, "a prompted action is not nullary");
    assert!(
        native
            .submit(&host.room, &source, MorningCommand::Refresh)
            .is_err()
    );
    assert!(
        native
            .submit(&host.room, &source, MorningCommand::DraftRefresh)
            .is_err()
    );
    assert!(host.commands.lock().unwrap().is_empty());
}

#[test]
fn morning_refresh_and_scheduling_draft_use_existing_room_bound_door_and_grant_nothing() {
    let (native, host, source) = fixture();
    native.payload(&host.room, &source);
    native.payload(&host.room, &source).unwrap();
    native
        .submit(&host.room, &source, MorningCommand::Refresh)
        .unwrap();
    assert!(
        native
            .command_status(&host.room, &source)
            .unwrap()
            .contains("accepted")
    );
    native.payload(&host.room, &source);
    native.payload(&host.room, &source).unwrap();
    native
        .submit(&host.room, &source, MorningCommand::DraftRefresh)
        .unwrap();
    let draft = {
        let requests = host.commands.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].target,
            application_action_target(&host.room, &source.app, &source.resource, "refresh")
        );
        assert_eq!(requests[1].target.address.action, "draft-shortcut");
        let argument = requests[1].argument.clone();
        drop(requests);
        let InvokeArgument::Json(arguments) = argument else {
            panic!("typed draft")
        };
        serde_json::from_value::<tilo_automation::Shortcut>(arguments["shortcut"].clone()).unwrap()
    };
    assert!(draft.grant.is_empty());
    assert!(draft.triggers.is_empty());
    assert!(draft.saved_by.is_none());
    let tilo_automation::StepKind::Invoke {
        resource, action, ..
    } = &draft.steps[0].kind
    else {
        panic!("exact source step")
    };
    assert_eq!(action.app, source.app);
    assert_eq!(action.action, "refresh");
    assert_eq!(resource, &tilo_automation::Selector::Fixed(source.resource));
}

#[test]
fn morning_refresh_schedule_uses_durable_existing_scheduler_without_duplicate_restart_fire() {
    use super::super::automation_triggers::{
        Armed, Clock, ManualClock, PersonalDevices, ScheduleTriggers, TriggerContext, TriggerEvent,
        TriggerSource,
    };
    let source = MorningSource {
        app: "so.tilo.fixture".into(),
        resource: "fixture:chosen".into(),
    };
    let mut draft = refresh_draft(&source);
    assert!(draft.triggers.is_empty());
    draft.triggers = vec![tilo_automation::TriggerSpec::Schedule {
        schedule: tilo_automation::Schedule::Calendar {
            at: tilo_automation::TimeOfDay {
                hour: 9,
                minute: 15,
            },
            weekdays: Vec::new(),
        },
        catch_up: tilo_automation::CatchUp::RunOnce,
    }];
    draft.saved_by = Some("synthetic".into());
    let clock = ManualClock::new(
        chrono::DateTime::parse_from_rfc3339("2026-10-11T09:14:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        chrono::FixedOffset::east_opt(0).unwrap(),
    );
    let devices = PersonalDevices::alone("synthetic-device");
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("schedule.json");
    let armed = [Armed {
        room: "synthetic-room".into(),
        shortcut: &draft,
        claims: &[],
    }];
    let mut schedules = ScheduleTriggers::open(Some(path.clone()));
    assert!(
        schedules
            .observe(
                TriggerEvent::Tick,
                &armed,
                TriggerContext {
                    clock: &clock,
                    devices: &devices
                }
            )
            .is_empty()
    );
    clock.advance(chrono::Duration::minutes(1));
    let mut fire = schedules.observe(
        TriggerEvent::Tick,
        &armed,
        TriggerContext {
            clock: &clock,
            devices: &devices,
        },
    );
    // The firing waits for the ledger the state writer puts on disk.
    crate::shell::durable_writer::global().flush();
    fire.extend(schedules.take_settled());
    assert_eq!(fire.len(), 1);
    assert_eq!(fire[0].room, "synthetic-room");
    assert_eq!(fire[0].shortcut, draft.id);
    assert_eq!(
        fire[0].claim.as_ref().unwrap().due_ms,
        clock.now().timestamp_millis()
    );
    assert!(path.exists(), "next fire persisted before delivery");
    let mut restarted = ScheduleTriggers::open(Some(path));
    assert!(
        restarted
            .observe(
                TriggerEvent::Tick,
                &armed,
                TriggerContext {
                    clock: &clock,
                    devices: &devices
                }
            )
            .is_empty()
    );
    clock.advance(chrono::Duration::days(1));
    assert!(
        restarted
            .observe(
                TriggerEvent::Tick,
                &[],
                TriggerContext {
                    clock: &clock,
                    devices: &devices
                }
            )
            .is_empty(),
        "deleted schedule is disarmed"
    );
}

#[test]
fn morning_payload_rejects_noisy_oversized_or_unrelated_provider_rows() {
    let (native, host, source) = fixture();
    let mut wrong = row();
    wrong.app_id = "so.tilo.another".into();
    let mut invalid = row();
    invalid.resource.presentation.as_mut().unwrap().preview = Some("a".repeat(513));
    *host.rows.lock().unwrap() = vec![wrong, invalid];
    native.payload(&host.room, &source);
    assert!(native.payload(&host.room, &source).is_none());
    *host.rows.lock().unwrap() = vec![row(); 33];
    native.entries.borrow_mut().clear();
    native.payload(&host.room, &source);
    assert!(
        native.payload(&host.room, &source).is_none(),
        "the provider must honor the bounded page"
    );
    assert!(host.commands.lock().unwrap().is_empty());
}

struct Hooks;
impl tilo_automation::RunHooks for Hooks {
    fn confirm(&self, _: tilo_automation::ConfirmationRequest) -> Pending<Option<Confirmation>> {
        Pending::ready(None)
    }
    fn ask(&self, _: tilo_automation::AskRequest) -> Pending<Option<serde_json::Value>> {
        Pending::ready(None)
    }
    fn settle(&self, _: tilo_actions::InvocationId) -> Pending<()> {
        Pending::ready(())
    }
}
#[test]
fn morning_scheduled_refresh_updates_the_same_chosen_payload_through_production_interpreter() {
    use super::super::automation_triggers::{
        Armed, ManualClock, PersonalDevices, ScheduleTriggers, TriggerContext, TriggerEvent,
        TriggerSource,
    };
    let (native, host, source) = fixture();
    native.payload(&host.room, &source);
    assert!(
        native
            .payload(&host.room, &source)
            .unwrap()
            .text()
            .contains("Two project goals")
    );
    let saved = saved_synthetic_refresh(&source);
    let clock = ManualClock::new(
        chrono::DateTime::parse_from_rfc3339("2026-10-11T09:59:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        chrono::FixedOffset::east_opt(0).unwrap(),
    );
    let devices = PersonalDevices::alone("synthetic-device");
    let armed = [Armed {
        room: host.room.clone(),
        shortcut: &saved,
        claims: &[],
    }];
    let mut schedule = ScheduleTriggers::open(None);
    assert!(
        schedule
            .observe(
                TriggerEvent::Tick,
                &armed,
                TriggerContext {
                    clock: &clock,
                    devices: &devices
                }
            )
            .is_empty()
    );
    clock.advance(chrono::Duration::minutes(1));
    let fired = schedule.observe(
        TriggerEvent::Tick,
        &armed,
        TriggerContext {
            clock: &clock,
            devices: &devices,
        },
    );
    assert_eq!(fired.len(), 1);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let report = runtime.block_on(tilo_automation::run(
        host.as_ref(),
        &Hooks,
        tilo_automation::RunRequest {
            shortcut: &saved,
            run: fired[0].run,
            caller: Caller::local_person("synthetic").in_room(&host.room),
            mode: tilo_automation::RunMode::Unattended,
            trigger: tilo_automation::RunTrigger::Fired(fired[0].trigger.clone()),
            inputs: fired[0].inputs.clone(),
            caused_by: None,
        },
    ));
    assert_eq!(report.outcome, tilo_automation::RunOutcome::Completed);
    assert_eq!(host.commands.lock().unwrap().len(), 1);
    native
        .entries
        .borrow_mut()
        .get_mut(&(host.room.clone(), source.key()))
        .unwrap()
        .checked = Instant::now()
        .checked_sub(CHECK_INTERVAL)
        .expect("the fixture clock can represent an expired catalogue");
    assert!(native.payload(&host.room, &source).is_none());
    assert!(
        native
            .payload(&host.room, &source)
            .unwrap()
            .text()
            .contains("Three project goals complete")
    );
    assert!(
        schedule
            .observe(
                TriggerEvent::Tick,
                &armed,
                TriggerContext {
                    clock: &clock,
                    devices: &devices
                }
            )
            .is_empty()
    );
}

fn saved_synthetic_refresh(source: &MorningSource) -> tilo_automation::Shortcut {
    let mut saved = refresh_draft(source);
    saved.saved_by = Some("synthetic".into());
    saved.grant = vec![tilo_automation::action_rule(
        &source.app,
        "fixture",
        Some(&source.resource),
        "refresh",
        tilo_action_policy::KindSet::READ,
        tilo_action_policy::Answer::Allow,
    )];
    saved.triggers = vec![tilo_automation::TriggerSpec::Schedule {
        schedule: tilo_automation::Schedule::Interval { seconds: 3600 },
        catch_up: tilo_automation::CatchUp::RunOnce,
    }];
    saved
}
