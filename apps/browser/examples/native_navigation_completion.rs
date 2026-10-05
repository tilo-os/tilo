//! Real Chromium and registered Browser effects, including same-document history.
//! Run explicitly with `cargo run -p browser --example native_navigation_completion`.
//! Builds the pinned engine through the maintained helper; never disables its sandbox.

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
                if stream.read(&mut request).is_err() {
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|argument| argument == "--built-host") {
        let built = std::process::Command::new("python3")
            .arg("scripts/browser/build-host.py")
            .status()?;
        if !built.success() {
            return Err("the maintained pinned Chromium host build failed".into());
        }
    }
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
            .status()?;
        return if status.success() {
            Ok(())
        } else {
            Err("unprivileged native fixture failed".into())
        };
    }
    if !std::env::args().any(|argument| argument == "--built-host") {
        let status = std::process::Command::new("python3")
            .arg("scripts/browser/run-native-fixture.py")
            .arg(std::env::current_exe()?)
            .arg("--built-host")
            .status()?;
        return if status.success() {
            Ok(())
        } else {
            Err("native fixture failed".into())
        };
    }
    let host = std::path::Path::new("target/chromium/tilo-chromium-host").canonicalize()?;
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(ui::webview::chromium::runtime::HOST_OVERRIDE_ENV, &host);
    let server = FixtureServer::start()?;
    let url = format!("http://{}/", server.address);
    let mut app = TestApp::<BrowserShell>::new()?;
    history_roundtrip(&mut app, &url)?;
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
    println!(
        "PASS: real Chromium same-document Back/Forward complete registered Browser invocation receipts before and after a genuine navigation failure"
    );
    Ok(())
}
