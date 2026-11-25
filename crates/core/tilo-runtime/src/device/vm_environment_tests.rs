use super::*;
use crate::RuntimeServices as _;
use crate::device::services::Daemon;
use crate::device::services::tests::isolated_root;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{ActionProvider, ReceiptStatus};

const THROUGH_THE_DISPATCH: &str = "device::vm_environment::tests::the_host_opens_a_window_in_the_machine_and_reads_it_through_the_dispatch";

/// `action` of the VM's sessions with `argument`, invoked through the dispatch `client` as `caller` and
/// followed until it answered: what it answered.
fn asked(
    client: &(dyn ActionProvider + Send + Sync),
    caller: &Caller,
    action: &str,
    argument: &impl serde::Serialize,
) -> Result<Value, String> {
    let mut request = InvokeRequest::plain(wire::target(action), serde_json::Map::new());
    request.argument = wire::argument(argument);
    request.wait_ms = Some(10_000);
    let receipt = client
        .invoke(caller, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the daemon answered")
        .map_err(|error| error.to_string())?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Accepted => {
            let began = std::time::Instant::now();
            while began.elapsed() < tilo_test_env::hang_bound() {
                match client
                    .observe(caller, &receipt.invocation)
                    .map(|state| state.update)
                {
                    Some(InvocationUpdate::Succeeded { output, .. }) => {
                        return Ok(output.value.unwrap_or_default());
                    }
                    Some(InvocationUpdate::Failed { error, .. }) => return Err(error.message),
                    _ => std::thread::sleep(Duration::from_millis(20)),
                }
            }
            Err("the call never ended".into())
        }
        other => Err(format!("{other:?}")),
    }
}

/// The host launches Browser on a record in the VM's daemon through the workspace's dispatch: the
/// session starts as one of the daemon's shells, takes the launch, and once it says its window is
/// up the host reads the window's provider; the apps session is a second shell beside it, and an
/// app that names no desktop file starts nothing. Ended at its limits, the last session ends the
/// daemon.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one named scenario walked through in order"
)]
fn the_host_opens_a_window_in_the_machine_and_reads_it_through_the_dispatch() {
    let Some(root) = isolated_root(THROUGH_THE_DISPATCH) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().unwrap();
    let started: Arc<Mutex<Vec<(Holds, String)>>> = Arc::default();
    let starts: Starts = {
        let started = Arc::clone(&started);
        Arc::new(move |holds, workspace| {
            started.lock_recover().push((holds, workspace.to_owned()));
            std::process::Command::new("sleep").arg("600").spawn()
        })
    };
    let ended = Arc::new(AtomicBool::new(false));
    let environment = VmEnvironment::new(
        starts,
        Limits {
            idle: Duration::from_secs(600),
            lifetime: Duration::from_secs(3600),
        },
        {
            let ended = Arc::clone(&ended);
            Box::new(move || ended.store(true, Ordering::SeqCst))
        },
    );
    provide(&daemon, &environment);
    // The device action service a device of the workspace asks this one on, as the host that
    // placed the workspace asks the VM's daemon; here the asking device is this one.
    let client = daemon.workspaces().device_actions();
    let client = client.as_ref();
    let caller = tilo_workspace_service::service::local_caller(daemon.workspaces().device_cert())
        .in_workspace(workspace.clone());
    // A session asks over the link the daemon hands the shells it starts, through the session's
    // own dispatch on it, as the person on this device.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let link = daemon.shell_link().unwrap();
    link.set_nonblocking(true).unwrap();
    let dispatch = runtime.block_on(async {
        let link = tokio::net::UnixStream::from_std(link).unwrap();
        let (shell, _) = runtime_collab::ShellClient::open(link);
        shell.dispatch()
    });
    let shell: &(dyn ActionProvider + Send + Sync) = &dispatch;
    let as_shell = Caller::local_person(daemon.persons_id()).in_workspace(workspace.clone());
    let window = |client: &(dyn ActionProvider + Send + Sync)| -> Window {
        serde_json::from_value(
            asked(client, &caller, wire::WINDOW, &serde_json::json!({})).unwrap(),
        )
        .unwrap()
    };
    assert_eq!(
        window(client),
        Window::default(),
        "nothing runs before a launch"
    );

    // The host launches Browser on a record: its session starts, for that workspace.
    let launch = Launch {
        workspace: workspace.clone(),
        holds: Holds::Browser,
        record: Some("uuid:5c07c075-0000-4000-8000-0000000000aa".into()),
        app: None,
    };
    let shown: Shown =
        serde_json::from_value(asked(client, &caller, wire::LAUNCH, &launch).unwrap()).unwrap();
    assert!(shown.running && shown.provider.is_none(), "{shown:?}");
    assert_eq!(
        *started.lock_recover(),
        [(Holds::Browser, workspace.clone())],
        "the session started as the daemon's shell, for the workspace launched in"
    );

    // The session takes what was launched for it, and the apps session takes nothing.
    let taken: Vec<Launch> = serde_json::from_value(
        asked(
            shell,
            &as_shell,
            wire::NEXT_LAUNCH,
            &Session {
                holds: Holds::Browser,
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(taken, std::slice::from_ref(&launch));
    assert!(environment.take(Holds::Apps).is_empty());

    // Once it says its window is up, the host reads the provider the window is listed under.
    asked(
        shell,
        &as_shell,
        wire::REPORT,
        &Report::Up {
            holds: Holds::Browser,
            provider: "42".into(),
        },
    )
    .unwrap();
    let read = window(client);
    assert_eq!(read.browser.provider.as_deref(), Some("42"));
    assert!(read.browser.running && !read.apps.running);

    // A page's fill asks the host for its vault item through the daemon, which asks as this
    // device: on a machine whose workspace is placed on no host nothing is released, and the
    // session is told why.
    let lent: ItemLent = serde_json::from_value(
        asked(
            shell,
            &as_shell,
            wire::USE_ITEM,
            &ItemUse {
                invocation: tilo_actions::InvocationId::new(),
                item: "vault.item:0123456789abcdef0123456789abcdef".into(),
                origin: "https://login.example.test".into(),
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        matches!(&lent, ItemLent::Refused { reason } if reason.contains("cannot reach")),
        "{lent:?}"
    );

    // Another device reaches none of the sessions' own asks.
    assert!(
        asked(
            client,
            &caller,
            wire::REPORT,
            &Report::Used {
                holds: Holds::Browser
            }
        )
        .is_err(),
        "a session's own ask is the VM's shells' alone"
    );

    // A second launch reaches the running session and starts no second one.
    asked(client, &caller, wire::LAUNCH, &launch).unwrap();
    assert_eq!(started.lock_recover().len(), 1, "one Browser session");

    // The Linux apps are a session of their own; an app that names no desktop file is refused.
    let mut app = Launch {
        workspace,
        holds: Holds::Apps,
        record: None,
        app: Some(AppLaunch::new("xterm").unwrap()),
    };
    asked(client, &caller, wire::LAUNCH, &app).unwrap();
    assert_eq!(window(client).apps.provider.as_deref(), Some(UP_PROVIDER));
    app.app = Some(AppLaunch {
        desktop_id: "sh -c id".into(),
    });
    assert!(asked(client, &caller, wire::LAUNCH, &app).is_err());
    assert_eq!(
        started
            .lock_recover()
            .iter()
            .map(|(holds, _)| *holds)
            .collect::<Vec<_>>(),
        [Holds::Browser, Holds::Apps]
    );
    // A launch in another workspace than the one asked in opens nothing.
    let elsewhere = Launch {
        workspace: "22222222-2222-4222-8222-222222222222".into(),
        ..launch
    };
    assert!(asked(client, &caller, wire::LAUNCH, &elsewhere).is_err());

    // Stopped, Browser's session runs no more; the apps session ends at its limits, and with the
    // last session the daemon is told to end.
    asked(
        client,
        &caller,
        wire::STOP,
        &Session {
            holds: Holds::Browser,
        },
    )
    .unwrap();
    assert!(!window(client).browser.running);
    assert!(!environment.keep_limits(tilo_clock::unix_now_ms()));
    assert!(!ended.load(Ordering::SeqCst), "the apps session still runs");
    assert!(environment.keep_limits(tilo_clock::unix_now_ms() + 3_600_000));
    assert!(ended.load(Ordering::SeqCst));
    assert_eq!(window(client), Window::default());
}
