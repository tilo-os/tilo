//! Real Chromium and registered Browser effects, including same-document history.
//! Run explicitly with `cargo run -p browser --example native_navigation_completion`.
//! Builds the pinned engine through the maintained helper; never disables its sandbox.
//! `--host PATH` instead exercises an existing runtime's exact host bytes.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use browser::effects::BrowserEffect;
use browser::resource::BrowserShell;
use ui::framework::testing::TestApp;

#[derive(Debug)]
struct InvocationFailure(ui::tilo_actions::ActionFailure);

impl std::fmt::Display for InvocationFailure {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(output, "{}: {}", self.0.code, self.0.message)
    }
}

impl std::error::Error for InvocationFailure {}

struct FixtureServer {
    address: std::net::SocketAddr,
    stopped: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl FixtureServer {
    fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let worker = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut request = [0; 2048];
                let Ok(read) = stream.read(&mut request) else {
                    continue;
                };
                if request
                    .get(..read)
                    .is_some_and(|request| request.starts_with(b"GET /redirect "))
                {
                    let _ = stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: /\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
                // A real page link adds same-document history after a trusted
                // click, so Chromium will retain it for Back/Forward.
                let body = b"<!doctype html><title>Navigation fixture</title><h1>First section</h1><a href='#second'>Second section</a>";
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(body);
            }
        });
        Ok(Self {
            address,
            stopped,
            worker: Some(worker),
        })
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn invoke(
    app: &mut TestApp<BrowserShell>,
    effect: &BrowserEffect,
) -> Result<ui::tilo_actions::ActionOutput, Box<dyn std::error::Error>> {
    use ui::tilo_resource::Effect;
    let instance = app
        .harness()
        .component_instance_id()
        .ok_or("missing Browser component")?;
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: instance,
        address: ui::tilo_actions::ActionAddress::new("browser.page", effect.id()),
    };
    let (invocation, started) =
        app.harness()
            .start_bound_job_for(None, &address, &effect.arguments(), false)?;
    if let Some(output) = started.completed() {
        return Ok(output);
    }
    app.harness()
        .wait_until(ui::time::Duration::from_secs(50), |harness| {
            harness
                .job_updates(invocation)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal)
        });
    let updates = app.harness().job_updates(invocation);
    println!(
        "{} receipt: {}",
        effect.id(),
        serde_json::to_string(updates)?
    );
    for update in updates {
        match update {
            ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                return Ok(output.clone());
            }
            ui::tilo_actions::InvocationUpdate::Failed { error, .. } => {
                return Err(Box::new(InvocationFailure(error.clone())));
            }
            _ => {}
        }
    }
    Err(format!(
        "{} did not succeed through its registered invocation",
        effect.id()
    )
    .into())
}

fn verify_equal<T: std::fmt::Debug + PartialEq + ?Sized>(
    observed: &T,
    expected: &T,
    context: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if observed != expected {
        return Err(format!("{context}: expected {expected:?}, observed {observed:?}").into());
    }
    Ok(())
}

fn explicit_blank_and_redirect(
    app: &mut TestApp<BrowserShell>,
    url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if invoke(
        app,
        &BrowserEffect::Navigate {
            url: "about:blank".into(),
        },
    )
    .is_ok()
    {
        return Err("Browser admitted an address outside its declared scheme boundary".into());
    }
    let redirected = invoke(
        app,
        &BrowserEffect::Navigate {
            url: format!("{url}redirect"),
        },
    )?;
    verify_equal(
        &redirected.value,
        &Some(serde_json::json!(url)),
        "Redirected URI",
    )?;
    let repeated = invoke(app, &BrowserEffect::Navigate { url: url.into() })?;
    verify_equal(
        &repeated.value,
        &Some(serde_json::json!(url)),
        "Repeated URI",
    )?;
    Ok(())
}

fn history_roundtrip(
    app: &mut TestApp<BrowserShell>,
    url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let history = app
        .person::<browser::history::BrowsingHistory>()
        .ok_or("missing ordinary Browser history record")?;
    let visits = |url: &str| {
        history.with(|state| {
            state
                .entries()
                .iter()
                .find(|entry| entry.url == url)
                .map_or(0, |entry| entry.visit_count)
        })
    };
    let before = visits(url);
    let fragment = format!("{url}#second");
    let fragment_before = visits(&fragment);
    let navigate = invoke(
        app,
        &BrowserEffect::Navigate {
            url: url.to_owned(),
        },
    )?;
    verify_equal(
        &navigate.value,
        &Some(serde_json::json!(url)),
        "Navigate URI",
    )?;
    invoke(
        app,
        &BrowserEffect::WaitPageSettled {
            quiet_ms: Some(100),
            timeout_ms: Some(2000),
        },
    )?;
    verify_equal(
        &visits(url),
        &before.checked_add(1).ok_or("history count overflow")?,
        "full navigation visit count",
    )?;
    let page = invoke(app, &BrowserEffect::ReadPage)?;
    let outline = page
        .value
        .as_ref()
        .and_then(|value| value.get("outline"))
        .and_then(serde_json::Value::as_str)
        .ok_or("missing fixture outline")?;
    let element = outline
        .split("[ref=")
        .nth(1)
        .and_then(|tail| tail.split(']').next())
        .ok_or("missing fixture link")?
        .to_owned();
    let clicked = invoke(app, &BrowserEffect::ClickPage { element })?;
    verify_equal(
        &clicked.value.as_ref().and_then(|value| value.get("url")),
        &Some(&serde_json::json!(format!("{url}#second"))),
        "Click URI",
    )?;
    let back = invoke(app, &BrowserEffect::Back)?;
    verify_equal(&back.value, &Some(serde_json::json!(url)), "Back URI")?;
    verify_equal(
        &visits(url),
        &before.checked_add(2).ok_or("history count overflow")?,
        "Back visit count",
    )?;
    let forward = invoke(app, &BrowserEffect::Forward)?;
    verify_equal(
        &forward.value,
        &Some(serde_json::json!(format!("{url}#second"))),
        "Forward URI",
    )?;
    verify_equal(
        &visits(&fragment),
        &fragment_before
            .checked_add(2)
            .ok_or("history count overflow")?,
        "fragment navigation visit count",
    )?;
    Ok(())
}

fn engine_outcome(
    app: &mut TestApp<BrowserShell>,
    engine: &ui::webview::SharedEngine,
    request: u32,
) -> Result<(String, Option<String>), Box<dyn std::error::Error>> {
    let mut outcome = None;
    app.harness()
        .wait_until(ui::time::Duration::from_secs(5), |_| {
            for event in engine.drain_events() {
                if let ui::webview::WebViewEvent::NavigationOutcome {
                    request_id,
                    uri,
                    error,
                } = event
                    && request_id == request
                {
                    outcome = Some((uri, error));
                }
            }
            outcome.is_some()
        });
    outcome.ok_or_else(|| format!("owned native navigation {request} did not complete").into())
}

fn site_reload_roundtrip(
    app: &mut TestApp<BrowserShell>,
    engine: &ui::webview::SharedEngine,
    url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use ui::webview::{WebViewCommand, WebViewEvent};
    engine.send(WebViewCommand::RequestSiteStatus {
        request_id: 700,
        origin: url.trim_end_matches('/').into(),
    });
    let mut status = None;
    app.harness()
        .wait_until(ui::time::Duration::from_secs(5), |_| {
            for event in engine.drain_events() {
                if let WebViewEvent::SiteStatus(value) = event
                    && value.request_id == 700
                {
                    status = Some(value);
                }
            }
            status.is_some()
        });
    let status = status.ok_or("native site status did not answer")?;
    engine.send(WebViewCommand::SiteAction {
        request_id: 701,
        origin: status.origin,
        generation: status.generation,
        action: "reload".into(),
    });
    let mut accepted = None;
    let mut loaded = false;
    app.harness()
        .wait_until(ui::time::Duration::from_secs(5), |_| {
            for event in engine.drain_events() {
                match event {
                    WebViewEvent::SiteResult {
                        request_id: 701,
                        success,
                        ..
                    } => accepted = Some(success),
                    WebViewEvent::LoadFinishedAt { uri } if uri == url => loaded = true,
                    _ => {}
                }
            }
            accepted == Some(false) || (accepted == Some(true) && loaded)
        });
    verify_equal(
        &(accepted, loaded),
        &(Some(true), true),
        "Scoped site reload",
    )?;
    Ok(())
}

fn engine_edge_cases(
    app: &mut TestApp<BrowserShell>,
    url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use ui::webview::{NavigationCommand, WebViewCommand};
    let engine: ui::webview::SharedEngine = ui::webview::chromium::ChromiumEngine::new()?;
    engine.navigate_owned(
        1,
        NavigationCommand::Navigate {
            url: "about:blank".into(),
        },
    )?;
    verify_equal(
        &engine_outcome(app, &engine, 1)?,
        &("about:blank".into(), None),
        "Owned explicit blank",
    )?;
    engine.navigate_owned(
        2,
        NavigationCommand::Navigate {
            url: url.replacen("http:", "HTTP:", 1),
        },
    )?;
    verify_equal(
        &engine_outcome(app, &engine, 2)?,
        &(url.into(), None),
        "Canonical owned target",
    )?;
    site_reload_roundtrip(app, &engine, url)?;
    engine.navigate_owned(
        3,
        NavigationCommand::Navigate {
            url: format!("{url}cancelled"),
        },
    )?;
    engine.send(WebViewCommand::CancelScopedNavigation { request_id: 3 });
    engine.navigate_owned(4, NavigationCommand::Navigate { url: url.into() })?;
    verify_equal(
        &engine_outcome(app, &engine, 4)?,
        &(url.into(), None),
        "Recovery after cancellation",
    )?;
    engine.navigate_owned(
        5,
        NavigationCommand::HistoryEntry {
            history_request_id: 1,
            id: i32::MAX as u32,
        },
    )?;
    if engine_outcome(app, &engine, 5)?.1.is_none() {
        return Err("unavailable history entry succeeded".into());
    }
    engine.navigate_owned(6, NavigationCommand::Navigate { url: url.into() })?;
    verify_equal(
        &engine_outcome(app, &engine, 6)?,
        &(url.into(), None),
        "Recovery after rejected history",
    )?;
    engine.navigate_owned(
        7,
        NavigationCommand::Navigate {
            url: "chrome://crash".into(),
        },
    )?;
    let (_, failure) = engine_outcome(app, &engine, 7)?;
    if !failure.is_some_and(|message| message.to_ascii_lowercase().contains("renderer")) {
        return Err("actual renderer termination did not fail its owned navigation".into());
    }
    engine.navigate_owned(8, NavigationCommand::Reload)?;
    let recovered = engine_outcome(app, &engine, 8)?;
    verify_equal(
        &recovered,
        &(url.into(), None),
        "Reload after renderer termination",
    )?;
    println!(
        "PASS: real native blank/canonical target/cancellation/history refusal/renderer termination and Reload recovery"
    );
    Ok(())
}

fn fixture_options() -> Result<(bool, Option<std::path::PathBuf>), Box<dyn std::error::Error>> {
    let mut built_host = false;
    let mut selected_host = None;
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--built-host" {
            built_host = true;
        } else if argument == "--host" {
            if selected_host.is_some() {
                return Err("the native fixture accepts one host path".into());
            }
            selected_host = Some(std::path::PathBuf::from(
                arguments.next().ok_or("--host requires a path")?,
            ));
        } else {
            return Err(format!(
                "unknown native fixture argument: {}",
                argument.to_string_lossy()
            )
            .into());
        }
    }
    Ok((built_host, selected_host))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (built_host, selected_host) = fixture_options()?;
    if !built_host && selected_host.is_none() {
        let built = std::process::Command::new("python3")
            .arg("scripts/browser/build-host.py")
            .status()?;
        if !built.success() {
            return Err("the maintained pinned Chromium host build failed".into());
        }
    }
    let host = selected_host
        .unwrap_or_else(|| std::path::PathBuf::from("target/chromium/tilo-chromium-host"))
        .canonicalize()?;
    if std::process::Command::new("id").arg("-u").output()?.stdout == b"0\n" {
        // Builder containers may inherit a root-only TMPDIR. The ordinary
        // test-environment wrapper creates the unprivileged profile under /tmp.
        let status = std::process::Command::new("runuser")
            .env("TMPDIR", "/tmp")
            .args([
                "--user",
                "nobody",
                "--",
                "bash",
                "tools/with-test-environment.sh",
            ])
            .args(["python3", "scripts/browser/run-native-fixture.py"])
            .arg(std::env::current_exe()?)
            .arg("--built-host")
            .arg("--host")
            .arg(&host)
            .status()?;
        return if status.success() {
            Ok(())
        } else {
            Err("unprivileged native fixture failed".into())
        };
    }
    if !built_host {
        let status = std::process::Command::new("python3")
            .arg("scripts/browser/run-native-fixture.py")
            .arg(std::env::current_exe()?)
            .arg("--built-host")
            .arg("--host")
            .arg(&host)
            .status()?;
        return if status.success() {
            Ok(())
        } else {
            Err("native fixture failed".into())
        };
    }
    let host_hash = hex::encode(Sha256::digest(std::fs::read(&host)?));
    println!("native fixture host SHA256: {host_hash}");
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(ui::webview::chromium::runtime::HOST_OVERRIDE_ENV, &host);
    let server = FixtureServer::start()?;
    let url = format!("http://{}/", server.address);
    let mut app = TestApp::<BrowserShell>::new()?;
    history_roundtrip(&mut app, &url)?;
    explicit_blank_and_redirect(&mut app, &url)?;
    // An overall loading-state end also follows errors. It must never turn
    // a genuine refused connection into a successful navigation receipt.
    let closed = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let refused_url = format!("http://{}/", closed.local_addr()?);
    drop(closed);
    let refused = invoke(&mut app, &BrowserEffect::Navigate { url: refused_url });
    let failure = refused
        .err()
        .ok_or("failed navigation was reported as successful")?;
    let failure = failure
        .downcast_ref::<InvocationFailure>()
        .ok_or("failed navigation returned no registered failure receipt")?;
    verify_equal(
        failure.0.code.as_str(),
        "browser.navigation_failed",
        "failure code",
    )?;
    verify_equal(
        failure.0.message.as_str(),
        "ERR_CONNECTION_REFUSED",
        "failure message",
    )?;
    history_roundtrip(&mut app, &url)?;
    engine_edge_cases(&mut app, &url)?;
    println!(
        "PASS: real Chromium same-document Back/Forward complete registered Browser invocation receipts before and after a genuine navigation failure"
    );
    Ok(())
}
