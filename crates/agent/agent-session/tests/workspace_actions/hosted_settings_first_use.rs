//! A browser chooses the workspace's first model on the hosted Settings page.
//! The agent worker renders the form, and the host holds its fixture provider's key.

use super::hosted_page_stream::{SignedPage, button, control};
use super::hosted_secret_use::logged;
use super::*;
use test_http::{Reply, Responder};
use tilo_actions::remote::{ActionCallError, ReceiptStatus, person_models};
use tilo_server_protocol::page::PageRefusal;

const KEY: &str = "hosted-settings-fixture-only-01982507";
const PROVIDER: &str = "Loopback fixture";
const MODEL: &str = "local-first-use";

/// Read the published service separately over the person's workspace session.
async fn models(browser: &mut Browser) -> person_models::ModelProviders {
    let target = person_models::target(person_models::LIST);
    let receipt = browser
        .invoke_settled(request(
            &target.provider,
            target.address,
            InvokeArgument::None,
        ))
        .await;
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the signed device can read the published model settings");
    };
    let listing = output.value.expect("the model service returns its listing");
    assert!(
        !listing.to_string().contains(KEY),
        "the listing exposed the provider key"
    );
    serde_json::from_value(listing).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signed_person_chooses_the_first_model_on_hosted_settings() {
    let lines = logged();
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "workspace host", DeviceCapabilities::member());
    let device = enrol(&root, "Ada's browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let workspaces = hosted.workspaces().unwrap();
    let workspace = workspaces.create("First use").await.unwrap().workspace;
    let mut browser = Browser::open_workspace(&hosted, &device, &host, &workspace).await;
    assert!(models(&mut browser).await.providers.is_empty());
    workspaces.call_private_model_addresses(true);
    let local = Responder::handle(|ask| {
        if ask.method != "GET" || ask.target != "/v1/models" {
            return Some(Reply::status(404));
        }
        if ask.header("authorization") != Some(&format!("Bearer {KEY}")) {
            return Some(Reply::status(401));
        }
        Some(
            Reply::status(200)
                .header("Content-Type", "application/json")
                .body(
                    serde_json::json!({"data": [{"id": "local-unused"}, {"id": MODEL}]})
                        .to_string(),
                ),
        )
    });
    let endpoint = format!("{}/v1", local.url());

    let mut page = SignedPage::open(
        &hosted,
        &device,
        &host,
        &workspace,
        "settings",
        "settings.section:agent_models",
    )
    .await
    .excluding(KEY);
    let other = page
        .until("Settings offers another provider", |html| {
            control(
                html,
                &[
                    "button",
                    "aria-label=\"Link another provider\"",
                    "data-a=\"activate\"",
                ],
                None,
            )
        })
        .await
        .unwrap();
    page.ask(&other).await;
    let (name, address, add) = page
        .until("Settings opens the provider form", |html| {
            Some((
                control(
                    html,
                    &["aria-label=\"Name\"", "data-a=\"value\""],
                    Some(PROVIDER),
                )?,
                control(
                    html,
                    &["aria-label=\"Address\"", "data-a=\"value\""],
                    Some(&endpoint),
                )?,
                button(html, "Link provider")?,
            ))
        })
        .await
        .unwrap();
    for act in [name, address, add] {
        page.ask(&act).await;
    }
    let (key, submit) = page
        .until("Settings presents the protected key field", |html| {
            Some((
                control(html, &["type=\"password\"", "data-a=\"value\""], Some(KEY))?,
                button(html, "Add key")?,
            ))
        })
        .await
        .unwrap();
    page.ask(&key).await;
    page.ask(&submit).await;
    let chooser = page
        .until("the page offers the first model chooser", |html| {
            control(
                html,
                &["role=\"combobox\"", "aria-haspopup=\"listbox\""],
                None,
            )
        })
        .await
        .unwrap();
    page.ask(&chooser).await;
    let choice = page
        .until("the chooser offers the local model", |html| {
            control(html, &["role=\"option\"", MODEL], None)
        })
        .await
        .unwrap();
    page.ask(&choice).await;
    page.until(
        "the page confirms the default and shows the work assignments",
        |html| {
            html.contains("What each kind of work runs on")
                .then_some(())
        },
    )
    .await
    .unwrap();
    let kept = models(&mut browser).await;
    let profile = kept
        .providers
        .iter()
        .find(|entry| entry.name == PROVIDER)
        .unwrap();
    assert_eq!(kept.default_provider.as_ref(), Some(&profile.id));
    assert_eq!(profile.endpoint, endpoint);
    assert!(profile.configured);
    assert_eq!(profile.default_model.as_deref(), Some(MODEL));

    // Both certificates belong to Ada. The program has its own grant; her browser signs the
    // service certificate with the device key it just proved to the host.
    let program = enrol(
        &root,
        "Ada's program",
        DeviceCapabilities::server_key_grant(),
    );
    let now = tilo_clock::unix_now_ns();
    let (key, identity) = tilo_user_identity::DeviceIdentity::generate("workspace service", now);
    let mut cert = tilo_user_identity::prepare_service_cert(
        &device.cert,
        identity.public_key,
        &identity.label,
        now,
    );
    tilo_user_identity::sign_device_cert(&mut cert, &device.key).unwrap();
    let service = Device { key, cert };
    for other in [program, service] {
        let mut page = SignedPage::open(
            &hosted,
            &other,
            &host,
            &workspace,
            "settings",
            "settings.section:agent_models",
        )
        .await
        .excluding(KEY);
        let denied = page
            .until(
                "Settings refuses the program or workspace service",
                |html| {
                    assert!(
                        !html.contains(PROVIDER) && !html.contains(MODEL),
                        "a non-person saw the person's models"
                    );
                    html.contains(&ActionCallError::NotPermitted.to_string())
                        .then_some(())
                },
            )
            .await;
        assert!(
            matches!(denied, Ok(()) | Err(PageRefusal::NotMember)),
            "expected a model or membership refusal"
        );

        let caller = tilo_workspace_service::service::local_caller(&other.cert)
            .in_workspace(workspace.clone());
        let remove = person_models::target(person_models::REMOVE_PROVIDER);
        let args = serde_json::Map::from_iter([("provider".into(), profile.id.clone().into())]);
        let mutation = request(&remove.provider, remove.address, InvokeArgument::Json(args));
        let refused = workspaces
            .page_model_setting(browser.workspace, &caller, &other.cert, &mutation)
            .await;
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "the model service must deny the mutation"
        );
        assert_eq!(models(&mut browser).await, kept);
    }

    let log = lines.0.lock().unwrap();
    let log = String::from_utf8_lossy(&log);
    assert!(
        log.contains("a page was acted on"),
        "the host's page activity was observed"
    );
    assert!(!log.contains(KEY), "host logs exposed the provider key");
}
