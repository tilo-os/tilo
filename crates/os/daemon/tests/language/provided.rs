//! A provider the device registers ([`Daemon::provide`](tilo_runtime::device::services::Daemon::provide))
//! answers every caller the same: a key's agent calling `tilo_do` on the device, and another
//! device of the person's over the device link (`tilo-actions/1`), each admitted by the
//! workspace's grants before the provider sees the call.

use std::sync::Arc;

use serde_json::json;
use tilo_actions::remote::TestProvider;
use tilo_actions::remote::{CataloguePage, CatalogueProvider, CatalogueRow, InvokeArgument};
use tilo_actions::{ActionAddress, ActionOutput, InvocationUpdate};

use crate::language::host;
use crate::language::host::said;

const PROVIDER: &str = "so.tilo.probe";

/// A provider offering `probe#ping`, answering each call with what it was asked to echo.
fn probe() -> Arc<TestProvider> {
    Arc::new(
        TestProvider::new()
            .with_catalogue(|caller, query| {
                let row = CatalogueRow::for_action(
                    PROVIDER,
                    PROVIDER,
                    tilo_actions::ActionDescriptor {
                        did: None,
                        state: tilo_actions::ActionState::default(),
                        semantic_identity: None,
                        address: ActionAddress::new("probe", "ping"),
                        resource_kind: "probe".into(),
                        resource_label: "Probe".into(),
                        action_label: "Ping".into(),
                        keywords: Vec::new(),
                        icon: None,
                        description: None,
                        locus: tilo_actions::ExecutionLocus::ClientLocal,
                        kind: tilo_actions::OperationKind::Effect,
                        contract: tilo_actions::ActionContract::default(),
                        input: tilo_actions::InputSpec::None,
                        params: vec![tilo_actions::ActionParam::required(
                            "echo",
                            tilo_actions::ParamType::Text,
                            "What the probe answers",
                        )],
                        view: None,
                    },
                );
                let mut page = CataloguePage::from_rows("1".into(), vec![row], &query);
                if query.provider.is_none() {
                    page.providers.push(CatalogueProvider {
                        provider: PROVIDER.into(),
                        app_id: PROVIDER.into(),
                        workspace: caller.workspace().unwrap_or_default().to_owned(),
                        document_id: String::new(),
                        browsable: true,
                    });
                }
                Ok(page)
            })
            .completing(|request| {
                let echo = match &request.argument {
                    InvokeArgument::Json(arguments) => arguments.get("echo").cloned(),
                    _ => None,
                };
                ActionOutput {
                    value: echo,
                    ..ActionOutput::default()
                }
            }),
    )
}

/// A key's agent on the device runs a registered provider's action with `tilo_do`, as
/// `tilo do -w` does: the device's dispatch hands it to the provider.
#[test]
fn a_registered_provider_answers_a_call_made_on_the_device() {
    host::run(|host| async move {
        let probe = probe();
        host.daemon().provide(PROVIDER, Arc::clone(&probe) as _);
        let workspace = host.workspace().to_owned();
        let key = host.key("Ada's script");

        let (done, failed) = said(
            &key.tool(
                "tilo_do",
                &json!({"workspace": workspace, "path": "probe#ping", "args": {"echo": "here"}}),
            )
            .await,
        );

        assert!(!failed, "{done}");
        assert!(done.contains("here"), "the provider's answer: {done}");
        let asked = probe.asked();
        assert_eq!(asked.len(), 1, "the provider was asked once: {asked:?}");
        assert_eq!(asked[0].target.provider, PROVIDER);
        // The call is the key's, as its run was admitted, not the person's at this desktop: the
        // workspace's grants and who is here see the key's own device.
        let callers = probe.callers();
        assert!(
            callers[0].device_public_key().is_some(),
            "the provider is called as the key's admitted run: {callers:?}"
        );
    });
}

/// Another device of the person's asks the same provider over the device link and is answered
/// the same.
#[test]
fn a_registered_provider_answers_the_same_call_from_another_device() {
    host::run(|host| async move {
        let probe = probe();
        host.daemon().provide(PROVIDER, Arc::clone(&probe) as _);
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;

        let answered = laptop
            .call(
                PROVIDER,
                ActionAddress::new("probe", "ping"),
                &json!({"echo": "there"}),
            )
            .await;

        let Ok(InvocationUpdate::Succeeded { output, .. }) = answered else {
            panic!("the laptop's call is answered: {answered:?}");
        };
        assert_eq!(output.value, Some(json!("there")));
        assert_eq!(probe.asked().len(), 1, "the provider was asked once");
    });
}
