//! An instance of the app whose invocation stream ended, as a closing window
//! ends it, is passed over: what was meant for it goes to another instance, or
//! launches one, rather than being refused.

use super::*;

tilo_test_env::scenario! {
    fn an_instance_whose_stream_ended_is_passed_over;

    phase a_stream_nobody_reads_is_never_picked;
    // A window of the app that went away left its stream unread: nothing
    // is addressed to it, and the invocation launches a window that takes
    // it. Before, it was picked and the invocation refused.
    {
        let (state, workspace) = broker(&workspace());
        let (streams, launches) = app_comes_up_as(&state, 8);
        drop(serve(&state, surface(7, &workspace)));
        let owner = Caller::local_person("owner").in_workspace(&workspace);
        let target = tilo_actions::remote::application_action_target(
            &workspace,
            APP,
            "files.app",
            "new-window",
        );
        invoke(&state, &owner, &target.provider, "new-window", None)
            .expect("delivered past the window that went");
        assert_eq!(*launches.lock().expect("launches"), [APP]);
        let mut window = streams.try_recv().expect("the new window came up");
        assert_eq!(window.try_recv().expect("it took it").action, "new-window");
    }

    phase a_stream_ending_after_the_pick_has_it_picked_again;
    // The window picked closes and ends its stream before the invocation
    // is admitted: the delivery looks again rather than refusing, and the
    // other window takes it.
    {
        let (state, workspace) = broker(&workspace());
        let _closing = serve(&state, surface(7, &workspace));
        let mut other = serve(&state, surface(8, &workspace));
        let owner = Caller::local_person("owner").in_workspace(&workspace);
        let target = tilo_actions::remote::application_action_target(
            &workspace,
            APP,
            "files.app",
            "new-window",
        );
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: target.provider.clone(),
                address: tilo_actions::ActionAddress::new("files.app", "new-window"),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let destination = state.admit_launchable(&owner, &request).expect("admitted");
        let picked = state
            .ready_delivery(APP, &workspace, None, &request.target.address)
            .expect("a window is ready");
        assert_eq!(picked.0, 7);
        let closing = state.invocation_stream(7).expect("its stream").downgrade();
        crate::collab_app_broker::InvocationFeedEnd {
            state: Arc::clone(&state),
            peer_id: 7,
            sender: closing,
        }
        .finish();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let ask = tilo_broker_core::delivery::DeliveryAsk {
            caller: &owner,
            origin: None,
            app_id: APP,
            workspace: &workspace,
            opened_on: destination.record.as_deref(),
            address: &request.target.address,
            start: false,
            timeout: state.launchable.delivery_timeout(),
            launches: super::launch_permits(),
        };
        let host = crate::collab_app_broker::action_provider::delivering::Delivering::new(&state, &request);
        let delivered = runtime.block_on(tilo_broker_core::delivery::deliver_picked(
            &state.publishers,
            &host,
            &ask,
            picked,
        ));
        assert!(
            delivered.is_none(),
            "the window that ended its stream is passed over, not refused for: {delivered:?}"
        );
        invoke(&state, &owner, &target.provider, "new-window", None)
            .expect("the other window takes it");
        assert_eq!(other.try_recv().expect("delivered").action, "new-window");
    }
}
