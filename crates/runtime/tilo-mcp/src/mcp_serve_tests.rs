//! What an MCP session answers, driven as the APIs drive it: the agents'
//! tools against a stub app, its directory, the content it produced and the
//! runs and executions the calls are attributed to.
use super::*;
use crate::test_hosts::{CatalogueAvailability, StubHost, run};

tilo_test_env::scenario! {
    fn default_activation_reaches_only_a_live_ready_owner;

    phase default_activation_rechecks_an_owner_that_exits_during_admission;
    {
        let host = StubHost {
            activation: true,
            availability: CatalogueAvailability::ExitDuringQuery,
            ..StubHost::default()
        };
        let invoked = host.invoked.clone();
        let server = McpServer::for_person(host, "workspace".into(), "owner");
        assert_eq!(run(server.activate_existing("so.tilo.test")).unwrap(), None);
        assert!(invoked.lock().unwrap().is_empty());
    }

    phase default_activation_uses_the_existing_permission_checked_view_api;
    {
        let host = StubHost {
            activation: true,
            ..StubHost::default()
        };
        let invoked = host.invoked.clone();
        let server = McpServer::for_person(host, "workspace".into(), "owner");
        run(async {
            assert_eq!(
                server.activate_existing("so.tilo.test").await.unwrap(),
                Some(71)
            );
        });
        let requests = invoked.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].target.provider, "71");
        assert_eq!(requests[0].target.address.resource, "test.note:1");
        assert_eq!(
            requests[0].target.address.action,
            tilo_actions::remote::APPLICATION_ACTIVATE_ACTION
        );
        drop(requests);
    }

    phase default_activation_does_not_confuse_other_records_or_absence_with_a_live_owner;
    {
        for host in [
            StubHost {
                activation: true,
                availability: CatalogueAvailability::Hidden,
                ..StubHost::default()
            },
            StubHost {
                activation: true,
                record: Some("uuid:another-record".into()),
                ..StubHost::default()
            },
        ] {
            let invoked = host.invoked.clone();
            let server = McpServer::for_person(host, "workspace".into(), "owner");
            run(async {
                assert_eq!(
                    server.activate_existing("so.tilo.test").await.unwrap(),
                    None
                );
            });
            assert!(invoked.lock().unwrap().is_empty());
        }
    }

    phase default_activation_refuses_unready_and_older_owners_without_starting_an_action;
    {
        for host in [
            StubHost::default(),
            StubHost {
                availability: CatalogueAvailability::NotPermitted,
                ..StubHost::default()
            },
        ] {
            let invoked = host.invoked.clone();
            let server = McpServer::for_person(host, "workspace".into(), "owner");
            run(async {
                assert!(server.activate_existing("so.tilo.test").await.is_err());
            });
            assert!(invoked.lock().unwrap().is_empty());
        }
    }

    phase a_permission_refusal_says_not_allowed_and_where_it_is_changed;
    // Whichever check refused the call, the workspace's permissions are named as the reason,
    // never an action the caller cannot name, with where the person changes them.
    {
        let said = String::from(BridgeError::Host(
            tilo_actions::remote::ActionCallError::NotPermitted,
        ));
        assert!(
            said == format!(
                "not_permitted: {}",
                tilo_actions::Availability::NOT_ALLOWED_HERE
            ) && said.contains("Workspace settings → Agents"),
            "{said}"
        );
    }
}

tilo_test_env::scenario! {
    fn the_devices_tool_lists_the_persons_other_devices;

    phase an_agent_lists_the_devices_its_host_reaches;
    {
        struct Devices;
        impl crate::devices::DeviceDirectory for Devices {
            fn reach(
                &self,
            ) -> std::pin::Pin<
                Box<dyn Future<Output = Vec<crate::devices::Reach>> + Send + '_>,
            > {
                let cert = |label: &str, key: u8| tilo_user_identity::DeviceCert {
                    user_public_key: [1; 32],
                    device_public_key: [key; 32],
                    device_label: label.into(),
                    capabilities: tilo_user_identity::DeviceCapabilities::member(),
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                    issuer: None,
                };
                Box::pin(async move {
                    vec![
                        crate::devices::Reach {
                            device: cert("Here", 1),
                            this: true,
                            answer: Ok(()),
                        },
                        crate::devices::Reach {
                            device: cert("Build box", 2),
                            this: false,
                            answer: Ok(()),
                        },
                    ]
                })
            }
        }
        let server = McpServer::for_agent(StubHost::default(), "workspace".into(), "agent", "owner")
            .with_devices(Some(std::sync::Arc::new(Devices)));
        run(async {
            let listed = server
                .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
                .await
                .unwrap();
            assert!(
                listed["result"]["tools"]
                    .as_array()
                    .is_some_and(|tools| tools
                        .iter()
                        .any(|tool| tool["name"] == crate::devices::DEVICES_TOOL)),
                "{listed}"
            );
            let called = server
                .handle(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                    "params":{"name": crate::devices::DEVICES_TOOL, "arguments": {}}}))
                .await
                .unwrap();
            let text = called["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default();
            assert!(text.contains("Build box"), "{called}");
            assert!(text.contains(&hex::encode([2_u8; 32])), "{called}");
        });
    }

    phase a_session_whose_host_reaches_no_device_lists_no_devices;
    {
        let server = McpServer::for_agent(StubHost::default(), "workspace".into(), "agent", "owner");
        run(async {
            let listed = server
                .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
                .await
                .unwrap();
            assert!(
                !listed["result"]["tools"]
                    .as_array()
                    .is_some_and(|tools| tools
                        .iter()
                        .any(|tool| tool["name"] == crate::devices::DEVICES_TOOL)),
                "{listed}"
            );
        });
    }
}
