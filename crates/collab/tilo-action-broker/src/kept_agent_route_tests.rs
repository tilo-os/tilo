/// A kept agent joins its own package's route in a journaled session, as the
/// canvas's ask hands it a turn: the session that keeps its state runs a
/// broker with a journal (`durable_owner_required`), whose join refuses every
/// application route it was not handed a private worker descriptor for. The
/// app the launcher started is the worker of its own route whichever principal
/// joins it, so the workspace's kept agent — which joins as `agent`, not as
/// its person — is admitted there too, and the workspace's record of its
/// agents is what admits the principal, separately.
#[tokio::test(flavor = "current_thread")]
async fn a_kept_agent_joins_its_own_route_in_a_journaled_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            const AGENT_PEER: u64 = 908;
            const AGENT: &str = "assistant";
            let env = Environment::new();
            let mut services = env.membership.services();
            // The workspace keeps `AGENT` for its owner: the roster the broker
            // asks before it admits an agent's join (`admit_kept_agent`).
            services.agents = Some(Arc::new(
                crate::test_support::KeptAgents::default().keeping(
                    &env.route.cluster_id,
                    AGENT,
                    &super::principal_id_for_root(env.cert.user_public_key),
                ),
            ));
            services.launcher = Some(Arc::new(LaunchedHere {
                app: env.route.app_id.clone(),
                workspace: env.route.cluster_id.clone(),
            }));
            let journal = tempfile::tempdir().unwrap();
            let config = crate::NativePeerBrokerConfig {
                journal_root: journal.path().join("journal"),
                certificate_authority: env.authority.clone(),
            };
            let rig = Rig::with_own_journal(&env, services, &config).await;
            assert!(rig.state.durable_owner_required);
            let (agent, _server, _client) = launched_app_connection(
                &rig.state,
                &rig.directories,
                AGENT_PEER,
                &env.route.app_id,
                &env.route.cluster_id,
            );
            let identity = presentation(&env.route.cluster_id, &env.cert, &env.key);
            let joined = agent
                .join_channel_as_kind(&env.route.channel_key(), &identity, AGENT, "agent", "")
                .await
                .expect("the workspace's kept agent joins its package's own route");
            // The route is held for the agent's connection, admitted as the
            // agent: a later join of it in this session is the same one.
            let _ = &joined;
            drop((joined, agent));
            rig.stop().await;
        })
        .await;
}
