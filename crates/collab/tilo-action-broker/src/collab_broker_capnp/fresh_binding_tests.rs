struct OneRowDirectory {
    row: runtime_collab::ActionDirectoryRow,
}

impl action_directory::Server for OneRowDirectory {
    fn query(
        self: Rc<Self>,
        _params: action_directory::QueryParams,
        mut results: action_directory::QueryResults,
    ) -> Promise<(), capnp::Error> {
        let page = runtime_collab::ActionDirectoryPage {
            revision: "fresh-binding-r1".into(),
            rows: vec![self.row.clone()],
            next_cursor: Vec::new(),
            exhausted: true,
            stale_cursor: false,
            budget_exhausted: false,
        };
        runtime_collab::write_action_directory_page(results.get().init_page(), &page)
            .expect("test directory page encodes");
        Promise::ok(())
    }

    fn resolve(
        self: Rc<Self>,
        _params: action_directory::ResolveParams,
        mut results: action_directory::ResolveResults,
    ) -> Promise<(), capnp::Error> {
        results.get().init_resolution().set_unavailable(());
        Promise::ok(())
    }
}

async fn query_row(
    fixture: &LocalDirectoryFixture,
    row: runtime_collab::ActionDirectoryRow,
) -> Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionHostError> {
    fixture
        .directories
        .directories
        .borrow_mut()
        .get_mut(&fixture.peer_id)
        .expect("fixture directory")
        .directory = capnp_rpc::new_client(OneRowDirectory { row });
    let caller = tilo_actions::remote::Caller::local_person("fixture-owner")
        .in_room(fixture.cluster_id.clone())
        .in_new_execution();
    let query = tilo_actions::remote::CatalogueQuery {
        provider: Some(fixture.peer_id.to_string()),
        max_results: 8,
        ..tilo_actions::remote::CatalogueQuery::default()
    };
    let mut pending = fixture.state.action_query(&caller, &query);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            drain_directory_bridge(fixture).await;
            if let tilo_actions::remote::PendingPoll::Ready(result) = pending.poll_ready() {
                break result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded directory query")
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_present_fresh_bindings_refuse_the_page_but_absence_remains_revision_bound() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = local_directory_fixture();
            fixture.directory_state.borrow_mut().fresh_binding = true;
            let valid = fixture.directory_state.borrow().row();
            let listed = query_row(&fixture, valid.clone())
                .await
                .expect("valid fresh row lists");
            assert!(
                listed
                    .rows
                    .first()
                    .expect("fresh row")
                    .fresh_binding
                    .is_some()
            );

            let mut absent = valid.clone();
            absent.fresh_binding = None;
            let listed = query_row(&fixture, absent)
                .await
                .expect("absent binding remains a revision-bound row");
            assert!(
                listed
                    .rows
                    .first()
                    .expect("revision-bound row")
                    .fresh_binding
                    .is_none()
            );

            let mut empty_locus = valid.clone();
            empty_locus
                .fresh_binding
                .as_mut()
                .expect("fresh binding")
                .locus
                .clear();
            assert!(query_row(&fixture, empty_locus).await.is_err());

            let mut mismatched = valid.clone();
            mismatched
                .fresh_binding
                .as_mut()
                .expect("fresh binding")
                .descriptor
                .action = "other".into();
            assert!(query_row(&fixture, mismatched).await.is_err());

            let mut oversized = valid.clone();
            oversized
                .fresh_binding
                .as_mut()
                .expect("fresh binding")
                .operation_type =
                Some("x".repeat(super::DIRECTORY_MAX_IDENTIFIER_BYTES.saturating_add(1)));
            assert!(query_row(&fixture, oversized).await.is_err());

            let mut container = valid;
            container.action = None;
            container.descriptor = None;
            container.has_children = true;
            assert!(query_row(&fixture, container).await.is_err());
        })
        .await;
}
