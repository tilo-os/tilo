//! Real certified HTTPS submit/retry and a canonical retained receipt.
use super::*;
use ui::feedback::{Destination, Draft, Submission};

async fn create_feedback_record(browser: &mut Browser) -> DocumentRoute {
    let route = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: browser.room.clone(),
        record: String::new(),
    };
    let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
    loop {
        browser.pump();
        if browser.documents.contains_key(&route) {
            break;
        }
        assert!(Instant::now() < deadline, "governor primary not received");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let source = browser.mount(&route);
    assert_eq!(
        source.wire_record(),
        "",
        "room-primary storage ids never cross the wire"
    );
    let mut doc = source.read(Clone::clone).unwrap();
    assert!(
        chat::projection::read(&doc)
            .workspace_rows()
            .iter()
            .any(|workspace| workspace.id == browser.room),
        "the production seed uses its host-selected room"
    );
    chat::demo::insert_category(
        &mut doc,
        "fixture",
        "feedback-category",
        &browser.room,
        "Feedback",
    )
    .unwrap();
    chat::demo::insert_channel(
        &mut doc,
        "fixture",
        "feedback",
        (&browser.room, "feedback-category"),
        "Feedback",
        chat::model::ChannelKind::Text,
    )
    .unwrap();
    let mutation = collab_types::DocumentMutation::Snapshot(doc.save());
    browser.peer.publish(route.clone(), &mutation).unwrap();
    let (collab_types::DocumentMutation::Snapshot(bytes)
    | collab_types::DocumentMutation::Incremental(bytes)) = mutation;
    browser
        .documents
        .entry(route.clone())
        .or_default()
        .push(([0; 32], bytes));
    route
}

fn approved(route: &DocumentRoute) -> Submission {
    let mut preview = Draft {
        text: "The book failed to open; token=secret12345".into(),
        // The fixture explicitly opts in to this field before reviewing it.
        context: vec![ui::feedback::ReviewText {
            node: "contact.name".into(),
            sensitivity: ui::feedback::DeclaredSensitivity::Personal,
            kind: Some(ui::feedback::RedactionCategory::Name),
            origin: ui::feedback::TextOrigin::Semantic,
            text: "Jana".into(),
        }],
    }
    .preview()
    .unwrap();
    assert!(!preview.feedback().text.contains("secret12345"));
    let reviewed = serde_json::to_string(preview.feedback()).unwrap();
    assert!(!reviewed.contains("Jana"));
    assert!(!reviewed.contains("contact.name"));
    preview.approve();
    preview
        .submission(
            "0123456789abcdef0123456789abcdef".into(),
            Destination {
                room: route.cluster_id.clone(),
                provider: record_provider(route),
                channel: "feedback".into(),
            },
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_reviewed_feedback_retries_once_and_reads_a_retained_receipt() {
    let (identity, _) = tilo_user_identity::UserIdentity::generate("Feedback visitor", 1);
    let host = enrol(
        &identity,
        "temporary-node",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let device = enrol(&identity, "temporary-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&identity, &host, state.path()).await;
    let room = hosted
        .relay
        .rooms()
        .unwrap()
        .create("Feedback fixture")
        .await
        .unwrap();
    let mut browser = Browser::open_room(&hosted, &device, &host, &room.room).await;
    let person = hex::encode(identity.verifying_key().to_bytes());
    hosted
        .relay
        .rooms()
        .unwrap()
        .prepare_application(
            &room.room,
            &host.cert,
            &common::catalogue(),
            "so.tilo.chat",
            "",
        )
        .await
        .unwrap();
    let route = create_feedback_record(&mut browser).await;
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let submission = approved(&route);
    let mut queued = participant(&mut browser, &route, &person);
    queued
        .invoke(
            &ActionAddress::new(
                chat::projection::item_id("channels", "feedback"),
                "submit-feedback",
            ),
            serde_json::json!({"submission": submission})
                .as_object()
                .unwrap(),
            false,
        )
        .unwrap();
    let mutation = queued.take_mutation().unwrap();
    browser.peer.close();
    drop(browser);
    browser = Browser::open_room(&hosted, &device, &host, &room.room).await;
    publish_participant(&mut browser, &route, &mutation);
    submit_local_participant(&mut browser, &route, &submission, &person);
    assert_signed_canonical(&mut browser, &submission, &host.cert, &device.cert).await;
    let read_address = ActionAddress::new(
        chat::projection::item_id("feedback", &submission.id),
        "read-feedback-receipt",
    );
    let output = retained_receipt(
        &mut browser,
        &submission.destination.provider,
        &read_address,
    )
    .await;
    let value = output.value.unwrap();
    assert_eq!(value["record"]["roomId"], room.room);
    assert_eq!(value["record"]["appId"], "so.tilo.chat");
    assert_eq!(value["record"]["recordId"], "");
    let receipt: ui::feedback::Receipt = serde_json::from_value(value["content"].clone()).unwrap();
    assert_eq!(receipt.id, submission.id);
    assert_eq!(receipt.digest, submission.digest().unwrap());
    assert_eq!(receipt.destination, submission.destination);
    let page = browser
        .catalogue(CatalogueQuery {
            provider: Some(record_provider(&route)),
            target: Some(read_address.clone()),
            max_results: 200,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        page.rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .any(|entry| entry.address == read_address)
    );
    let mut current = request(
        &submission.destination.provider,
        read_address.clone(),
        InvokeArgument::None,
    );
    current.revision = Some(page.revision.clone());
    assert!(matches!(
        browser.invoke(current.clone()).await.status,
        ReceiptStatus::Completed { .. }
    ));
    current.revision = Some("stale-source".into());
    assert!(matches!(
        browser.invoke(current).await.status,
        ReceiptStatus::Rejected { .. }
    ));
    assert_one_report(&mut browser, &route, &submission, &person).await;
    assert_local_only_report_is_not_a_receipt(&mut browser, &route, &submission).await;
    let mut maintainer = participant(&mut browser, &route, &person);
    maintainer
        .invoke(
            &ActionAddress::new(
                chat::projection::item_id("feedback", &submission.id),
                "triage",
            ),
            serde_json::json!({"update":{"status":"resolved"}})
                .as_object()
                .unwrap(),
            false,
        )
        .unwrap();
    publish_participant(&mut browser, &route, &maintainer.take_mutation().unwrap());
    let before = resolved_status(&mut browser, &submission).await;
    browser.peer.close();
    drop(browser);
    drop(maintainer);
    drop(queued);
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
    let restored = start(&identity, &host, state.path()).await;
    let mut returning = Browser::open_room(&restored, &device, &host, &room.room).await;
    let after = resolved_status(&mut returning, &submission).await;
    assert_signed_canonical(&mut returning, &submission, &host.cert, &device.cert).await;
    assert_eq!(
        before["content"], after["content"],
        "cold canonical triage must survive host restart"
    );
    assert_eq!(
        after["content"]["receipt"]["digest"],
        submission.digest().unwrap()
    );
    assert_eq!(after["content"]["triage"][0]["author"], person);
    assert_one_report(&mut returning, &route, &submission, &person).await;
    returning.peer.close();
    drop(restored.web);
    restored.agents.shutdown();
    restored.relay.shutdown().await;
}

/// A commenter files a report through the owner's submit action: it runs
/// there under their own standing and answers only once the owner's durable
/// store holds it. Filing again keeps one report; a Write action such as
/// triage stays refused to them, as does anything outside `comments`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_commenter_files_through_the_owner_and_cannot_triage() {
    use super::resource_support::{admit_person, open_admitted_peer};
    use tilo_peer_state::join::MemberRole;
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Feedback host", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Feedback guest", 1);
    let host = enrol(
        &owner,
        "feedback-node",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let owner_device = enrol(&owner, "owner-browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&owner, &host, state.path()).await;
    let room = hosted
        .relay
        .rooms()
        .unwrap()
        .create("Commenter feedback")
        .await
        .unwrap();
    hosted
        .relay
        .rooms()
        .unwrap()
        .prepare_application(
            &room.room,
            &host.cert,
            &common::catalogue(),
            "so.tilo.chat",
            "",
        )
        .await
        .unwrap();
    let mut owner_browser = Browser::open_room(&hosted, &owner_device, &host, &room.room).await;
    let route = create_feedback_record(&mut owner_browser).await;
    owner_browser
        .peer
        .barrier()
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    admit_person(
        &hosted,
        &host,
        &room.room,
        &guest_device,
        MemberRole::Commenter,
    )
    .await;
    let mut visitor = open_admitted_peer(&hosted, &guest_device, &host, &room.room).await;
    let submission = approved(&route);
    let submit = || {
        request(
            &submission.destination.provider,
            ActionAddress::new(
                chat::projection::item_id("channels", "feedback"),
                "submit-feedback",
            ),
            InvokeArgument::Json(
                serde_json::json!({"submission": submission})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
    };
    let filed = filed_once(&mut visitor, submit()).await;
    assert!(
        matches!(filed, ReceiptStatus::Completed { .. }),
        "the owner files a commenter's report: {filed:?}"
    );
    // Answered only after the owner's durable store holds it: the signed
    // receipt is there at once, not after a wait.
    let guest_person = hex::encode(guest_device.cert.user_public_key);
    let answer = owner_feedback(&mut visitor, &submission, true)
        .await
        .unwrap();
    assert!(answer.receipt.is_some(), "a filed report has its receipt");
    assert!(matches!(
        visitor.invoke(submit()).await.status,
        ReceiptStatus::Completed { .. }
    ));
    assert_one_report(&mut owner_browser, &route, &submission, &guest_person).await;
    let triage = visitor
        .invoke(request(
            &submission.destination.provider,
            ActionAddress::new(
                chat::projection::item_id("feedback", &submission.id),
                "triage",
            ),
            InvokeArgument::Json(
                serde_json::json!({"update":{"status":"resolved"}})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        ))
        .await;
    assert!(
        !matches!(triage.status, ReceiptStatus::Completed { .. }),
        "triage needs Write, which a commenter does not have: {triage:?}"
    );
    visitor.peer.close();
    owner_browser.peer.close();
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}

/// File a report, waiting out an owner still joining the room.
async fn filed_once(browser: &mut Browser, request: InvokeRequest) -> ReceiptStatus {
    let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
    loop {
        let mut attempt = request.clone();
        attempt.invocation = tilo_actions::InvocationId::new();
        let receipt = browser.invoke(attempt).await;
        match receipt.status {
            ReceiptStatus::Rejected {
                error: ActionHostError::Unavailable { .. },
            } if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            status => return status,
        }
    }
}

async fn retained_receipt(
    browser: &mut Browser,
    provider: &str,
    address: &ActionAddress,
) -> tilo_actions::ActionOutput {
    let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
    loop {
        let receipt = browser
            .invoke(request(provider, address.clone(), InvokeArgument::None))
            .await;
        if let ReceiptStatus::Completed { output } = receipt.status {
            return output;
        }
        assert!(
            Instant::now() < deadline,
            "canonical receipt unavailable: {receipt:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn assert_one_report(
    browser: &mut Browser,
    route: &DocumentRoute,
    submission: &Submission,
    person: &str,
) {
    let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
    loop {
        let replica = browser.mount(route);
        let reports = replica.read(chat::feedback::reports).unwrap().unwrap();
        if !reports.is_empty() {
            assert_eq!(reports.len(), 1);
            let report = reports.first().unwrap();
            assert_eq!(&report.submission, submission);
            assert_eq!(report.author, person);
            assert_eq!(
                replica.read(|doc| chat::projection::read(doc)
                    .message_rows(&chat::messages::Messages::default(), "feedback")
                    .len()),
                Some(1)
            );
            return;
        }
        assert!(Instant::now() < deadline, "signed report not received");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn assert_local_only_report_is_not_a_receipt(
    browser: &mut Browser,
    route: &DocumentRoute,
    submission: &Submission,
) {
    let mut local = browser.mount(route);
    let mut unsubmitted = submission.clone();
    unsubmitted.id = "1123456789abcdef0123456789abcdef".into();
    local
        .invoke(
            &ActionAddress::new(
                chat::projection::item_id("channels", "feedback"),
                "submit-feedback",
            ),
            serde_json::json!({"submission": unsubmitted})
                .as_object()
                .unwrap(),
            false,
        )
        .unwrap();
    assert!(
        local
            .read(|doc| chat::feedback::report(doc, &unsubmitted.id))
            .unwrap()
            .unwrap()
            .is_some()
    );
    let receipt = browser
        .invoke(request(
            &submission.destination.provider,
            ActionAddress::new(
                chat::projection::item_id("feedback", &unsubmitted.id),
                "read-feedback-receipt",
            ),
            InvokeArgument::None,
        ))
        .await;
    assert!(
        !matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "a private replica is not a retained destination receipt"
    );
}

fn submit_local_participant(
    browser: &mut Browser,
    route: &DocumentRoute,
    submission: &Submission,
    person: &str,
) {
    let mut replica = participant(browser, route, person);
    replica
        .invoke(
            &ActionAddress::new(
                chat::projection::item_id("channels", "feedback"),
                "submit-feedback",
            ),
            serde_json::json!({"submission": submission})
                .as_object()
                .unwrap(),
            false,
        )
        .unwrap();
    if let Some(mutation) = replica.take_mutation() {
        publish_participant(browser, route, &mutation);
    }
}

fn participant(browser: &mut Browser, route: &DocumentRoute, person: &str) -> Replica {
    browser.pump();
    let changes = browser
        .documents
        .get(route)
        .unwrap()
        .iter()
        .map(|(_, bytes)| bytes.clone())
        .collect();
    Replica::open(
        &common::catalogue(),
        &route.app_id,
        &route.cluster_id,
        person,
        tilo_agent_participant::ReplicaActor::Owner,
        Seed::Existing {
            record: ui::record::RecordId::package_primary(),
            changes,
        },
    )
    .unwrap()
}

fn publish_participant(
    browser: &mut Browser,
    route: &DocumentRoute,
    mutation: &collab_types::DocumentMutation,
) {
    browser.peer.publish(route.clone(), mutation).unwrap();
    let (collab_types::DocumentMutation::Snapshot(bytes)
    | collab_types::DocumentMutation::Incremental(bytes)) = mutation;
    browser
        .documents
        .entry(route.clone())
        .or_default()
        .push(([0; 32], bytes.clone()));
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
}

async fn resolved_status(browser: &mut Browser, submission: &Submission) -> serde_json::Value {
    let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
    loop {
        let result = browser
            .invoke(request(
                &submission.destination.provider,
                ActionAddress::new(
                    chat::projection::item_id("feedback", &submission.id),
                    "read-feedback-status",
                ),
                InvokeArgument::None,
            ))
            .await;
        if let ReceiptStatus::Completed { output } = result.status {
            let value = output.value.unwrap();
            if value["content"]["triage"][0]["update"]["status"] == "resolved" {
                assert_eq!(value["record"]["roomId"], submission.destination.room);
                assert_eq!(value["record"]["appId"], "so.tilo.chat");
                assert_eq!(value["record"]["recordId"], "");
                return value;
            }
        }
        assert!(
            Instant::now() < deadline,
            "canonical resolved feedback unavailable"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn owner_feedback(
    browser: &mut Browser,
    submission: &Submission,
    include_report: bool,
) -> Result<tilo_actions::remote::feedback::FeedbackAnswer, ActionHostError> {
    use tilo_actions::remote::feedback::{FeedbackDestination, FeedbackQuery};
    browser.correlation = browser.correlation.checked_add(1).unwrap();
    let correlation = browser.correlation;
    browser.send(RemoteActionMessage::FeedbackRequest {
        correlation,
        query: FeedbackQuery {
            destination: FeedbackDestination {
                room: submission.destination.room.clone(),
                provider: submission.destination.provider.clone(),
                channel: submission.destination.channel.clone(),
            },
            submission: include_report.then(|| serde_json::to_value(submission).unwrap()),
        },
    });
    browser
        .answer("signed canonical feedback", |message| match message {
            RemoteActionMessage::FeedbackResponse {
                correlation: received,
                result,
            } if *received == correlation => Some(result.clone()),
            _ => None,
        })
        .await
}
async fn assert_signed_canonical(
    browser: &mut Browser,
    submission: &Submission,
    owner: &tilo_user_identity::DeviceCert,
    reporter: &tilo_user_identity::DeviceCert,
) {
    use tilo_actions::remote::Caller;
    use tilo_desktop::feedback::transport::{Enrollment, SignedReceipt};
    let admission = owner_feedback(browser, submission, false).await.unwrap();
    assert!(
        admission.receipt.is_none(),
        "admission must not fabricate report persistence"
    );
    assert_eq!(
        admission.admission.owner_device,
        hex::encode(owner.device_public_key)
    );
    assert_eq!(
        admission.admission.reporter_device,
        hex::encode(reporter.device_public_key)
    );
    let enrollment = Enrollment {
        destination: submission.destination.clone(),
        owner: Caller::authenticated(owner.user_public_key, owner.device_public_key)
            .in_room(&submission.destination.room),
        reporter: Caller::authenticated(reporter.user_public_key, reporter.device_public_key)
            .in_room(&submission.destination.room),
        permissions: {
            let standing: ui::permissions::Standing =
                serde_json::from_value(admission.admission.standing).unwrap();
            assert_eq!(standing.person, hex::encode(reporter.user_public_key));
            ui::permissions::PermissionContext::new(
                ui::permissions::Actor::person(standing.person.clone())
                    .in_room(&submission.destination.room),
                serde_json::from_value(admission.admission.sandbox).unwrap(),
                standing,
            )
            .at_unix_time(admission.admission.unix_time)
        },
    };
    let answer = owner_feedback(browser, submission, true).await.unwrap();
    let receipt =
        SignedReceipt::decode(&serde_json::to_vec(&answer.receipt.unwrap()).unwrap()).unwrap();
    assert_eq!(
        receipt.verify(submission, &enrollment).unwrap().digest,
        submission.digest().unwrap()
    );
    let mut changed = submission.clone();
    changed.feedback.text.push_str(" changed approved content");
    assert!(receipt.verify(&changed, &enrollment).is_err());
    assert!(owner_feedback(browser, &changed, true).await.is_err());
    let mut absent = submission.clone();
    absent.id = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into();
    assert!(
        owner_feedback(browser, &absent, true).await.is_err(),
        "a locally approved but unretained report has no canonical acknowledgment"
    );
    let mut wrong_destination = submission.clone();
    wrong_destination.destination.channel = "missing-existing-destination".into();
    assert!(
        owner_feedback(browser, &wrong_destination, false)
            .await
            .is_err()
    );
}
