fn receipt_record(rig: &Rig, wire: u64) -> DurableInvocation {
    let key = rig
        .state
        .pending_invocations
        .lock()
        .unwrap()
        .get(&wire)
        .expect("invocation remains pending")
        .journal_key
        .clone()
        .expect("automatic journal admission");
    rig.state
        .journal
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .lookup(&key)
        .unwrap()
        .expect("admission retained")
}

fn receipt_barriers(rig: &Rig) -> invocation_journal::Barriers {
    rig.state
        .journal
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .barriers()
}

#[tokio::test(flavor = "current_thread")]
async fn actor_only_local_dispatch_never_receives_certified_evidence() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let row = directory_state("original-r1").borrow().row();
            rig.worker
                .handle
                .publish_actions(&env.route.app_id, &[row.descriptor.unwrap()])
                .await
                .unwrap();
            let actor = principal_id_for_root(env.cert.user_public_key);
            let job = rig
                .state
                .start_invocation_confirmed(903, "workspace:atlas", "open", None, &actor, None)
                .expect("legacy local read remains supported");
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            let record = receipt_record(&rig, delivered.id);
            assert!(
                record.admission.is_none(),
                "an actor string is not authenticated custody"
            );
            assert!(record.terminal.is_none());
            drop(job);
            rig.stop().await;
        })
        .await;
}
