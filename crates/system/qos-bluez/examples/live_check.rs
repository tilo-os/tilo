//! Live check against the running `bluetoothd` on this machine.
//!
//! This is a tool, not a test: it is deliberately outside the test suite so
//! it can insist on observing real hardware instead of skipping when the bus
//! is absent. Every failure — no system bus, no adapter, an agent BlueZ
//! refuses to register — exits non-zero.
//!
//! ```text
//! cargo run -p qos-bluez --example live_check                 # adapter + agent round trip
//! cargo run -p qos-bluez --example live_check -- --scan 15    # ...and scan for 15s
//! cargo run -p qos-bluez --example live_check -- --pair AA:BB:CC:DD:EE:FF
//! ```
//!
//! `--pair` runs the real pairing flow and answers the agent from the
//! terminal, which is what a compositor prompt does with a mouse.

use std::io::{self, BufRead, Write};
use std::time::Duration;

use qos_bluez::agent::{AgentEvent, AgentRegistration, PairingAgent};
use qos_bluez::model::{PairingPrompt, PairingReply};
use qos_bluez::BluezClient;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("\nFAIL: {e}");
        std::process::exit(1);
    }
}

fn arg_value(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

struct CallResult {
    ok: bool,
    text: String,
}

/// Invoke a method on our own `org.bluez.Agent1` object **as root**, the way
/// `bluetoothd` does.
///
/// Only root may send to that interface (see the comment at the call site),
/// so this is the sole way to exercise the agent without a device physically
/// pairing. Failure to become root is a hard error — the check must never
/// quietly degrade into not testing the agent at all.
async fn as_root_agent_call(
    unique_name: &str,
    path: &str,
    method: &str,
    signature_and_args: &[&str],
) -> Result<CallResult, String> {
    // Blocking on the runtime here would starve the task that answers the
    // prompt, and the call would "time out" against our own agent.
    let unique_name = unique_name.to_string();
    let path = path.to_string();
    let method = method.to_string();
    let args: Vec<String> = signature_and_args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || run_busctl(&unique_name, &path, &method, &args))
        .await
        .map_err(|e| e.to_string())?
}

fn run_busctl(
    unique_name: &str,
    path: &str,
    method: &str,
    signature_and_args: &[String],
) -> Result<CallResult, String> {
    let mut cmd = std::process::Command::new("sudo");
    cmd.arg("-n")
        .arg("busctl")
        .arg("--system")
        .arg("call")
        .arg(unique_name)
        .arg(path)
        .arg("org.bluez.Agent1")
        .arg(method);
    for a in signature_and_args {
        cmd.arg(a);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run `sudo busctl` to stand in for bluetoothd: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
    .trim()
    .to_string();
    if text.contains("a password is required") || text.contains("sudo:") && !text.contains("org.") {
        return Err(format!(
            "could not become root to stand in for bluetoothd: {text}"
        ));
    }
    Ok(CallResult {
        ok: out.status.success(),
        text,
    })
}

async fn run() -> Result<(), String> {
    let scan_secs: u64 = arg_value("--scan")
        .map(|s| s.parse().map_err(|_| "--scan wants a number of seconds".to_string()))
        .transpose()?
        .unwrap_or(0);
    let pair_target = arg_value("--pair");

    // 1. Reach bluetoothd.
    let client = BluezClient::connect()
        .await
        .map_err(|e| format!("cannot reach org.bluez on the system bus: {e}"))?;
    let adapter = client
        .default_adapter()
        .await
        .map_err(|e| format!("cannot enumerate adapters: {e}"))?
        .ok_or("no Bluetooth adapter on this machine")?;
    println!(
        "adapter  {}  [{}]  name={:?} powered={} discovering={}",
        adapter.path, adapter.address, adapter.name, adapter.powered, adapter.discovering
    );

    // 2. Known devices.
    let devices = client.devices().await.map_err(|e| format!("cannot list devices: {e}"))?;
    println!("devices  {} known", devices.len());
    for d in &devices {
        println!(
            "  {:<20} {} paired={} trusted={} connected={} rssi={:?}",
            d.name, d.address, d.paired, d.trusted, d.connected, d.rssi
        );
    }

    // 3. Register the pairing agent with the real daemon.
    let names = client.names();
    let (agent, mut events) = PairingAgent::new(names);
    let reg = AgentRegistration::register(client.connection(), agent)
        .await
        .map_err(|e| format!("bluetoothd refused to register the pairing agent: {e}"))?;
    println!("agent    registered at {} and requested as default", reg.path());

    // 4. Prove the agent answers a passkey question over the real bus.
    //
    //    The caller has to be root. Fedora's `bluetooth.conf` grants
    //    `send_interface="org.bluez.Agent1"` to the root policy only, so the
    //    single thing on the machine allowed to invoke a pairing agent is
    //    `bluetoothd` itself. That rejection is the defence, not a bug: it is
    //    what stops any user process from driving the pairing dialog. So the
    //    check goes through the same gate, with `busctl` as the root sender
    //    standing in for the daemon — the message our agent receives is
    //    identical.
    let unique = client
        .connection()
        .unique_name()
        .ok_or("our connection has no unique name")?
        .to_string();
    let prompt_task = tokio::spawn(async move {
        let ev = events.recv().await.ok_or("agent channel closed")?;
        let AgentEvent::Ask { request, reply } = ev else {
            return Err("expected an Ask event".to_string());
        };
        println!(
            "prompt   \"{}\" asks: {:?} (passkey text {:?})",
            request.device_name,
            request.prompt,
            request.prompt.passkey_text()
        );
        if request.prompt != (PairingPrompt::ConfirmPasskey { passkey: 424_242 }) {
            return Err(format!("unexpected prompt: {:?}", request.prompt));
        }
        reply
            .send(PairingReply::Accept)
            .map_err(|_| "could not answer the prompt".to_string())?;
        Ok::<_, String>(request.device_name)
    });

    const DEV: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF";
    let call = as_root_agent_call(&unique, qos_bluez::AGENT_PATH, "RequestConfirmation", &[
        "ou", DEV, "424242",
    ])
    .await?;
    if !call.ok {
        return Err(format!(
            "agent rejected a passkey confirmation it should have accepted: {}",
            call.text
        ));
    }
    let named = prompt_task.await.map_err(|e| e.to_string())??;
    println!("agent    RequestConfirmation from root round-tripped (device {named})");

    // Prove the policy really is the gate: the same call from *this*
    // unprivileged process must be refused by the bus before it ever reaches
    // our object.
    let caller = zbus::Connection::system()
        .await
        .map_err(|e| format!("second system-bus connection failed: {e}"))?;
    let agent_proxy = zbus::Proxy::new(&caller, unique.as_str(), qos_bluez::AGENT_PATH, "org.bluez.Agent1")
        .await
        .map_err(|e| format!("cannot address our own agent object: {e}"))?;
    let device = zbus::zvariant::ObjectPath::try_from(DEV).map_err(|e| e.to_string())?;
    match agent_proxy.call_method("RequestConfirmation", &(&device, 1u32)).await {
        Ok(_) => {
            return Err(
                "an unprivileged process drove the pairing agent — the bus policy that \
                 restricts org.bluez.Agent1 to root is not in force"
                    .to_string(),
            )
        }
        Err(zbus::Error::MethodError(name, _, _))
            if name.as_str() == "org.freedesktop.DBus.Error.AccessDenied" =>
        {
            println!("agent    unprivileged callers are refused by bus policy (only bluetoothd may ask)");
        }
        Err(e) => return Err(format!("unprivileged call failed for the wrong reason: {e}")),
    }

    // And prove a refusal really refuses: this call must come back as
    // org.bluez.Error.Rejected, not succeed.
    //
    // On its own connection, because BlueZ keys agents by sender and refuses
    // a second RegisterAgent from the same one with AlreadyExists.
    let conn2 = zbus::Connection::system()
        .await
        .map_err(|e| format!("third system-bus connection failed: {e}"))?;
    let unique2 = conn2
        .unique_name()
        .ok_or("the second connection has no unique name")?
        .to_string();
    let (agent2, mut events2) = PairingAgent::with_timeout(client.names(), Duration::from_secs(5));
    let reg2 = AgentRegistration::register_at(
        &conn2,
        agent2,
        "/org/qos/BluetoothAgentRefuseCheck",
        "KeyboardDisplay",
    )
    .await
    .map_err(|e| format!("cannot register the second agent: {e}"))?;
    tokio::spawn(async move {
        if let Some(AgentEvent::Ask { reply, .. }) = events2.recv().await {
            let _ = reply.send(PairingReply::Reject);
        }
    });
    let refused = as_root_agent_call(
        &unique2,
        "/org/qos/BluetoothAgentRefuseCheck",
        "RequestConfirmation",
        &["ou", DEV, "1"],
    )
    .await?;
    if refused.ok {
        return Err("a declined pairing returned success".to_string());
    }
    if !refused.text.contains("org.bluez.Error.Rejected") {
        return Err(format!("declining returned the wrong error: {}", refused.text));
    }
    println!("agent    declining returns org.bluez.Error.Rejected");
    reg2.unregister().await.map_err(|e| e.to_string())?;

    // 5. Real discovery, if asked.
    if scan_secs > 0 {
        println!("scan     starting {scan_secs}s discovery on {}", adapter.path);
        client
            .start_discovery(&adapter.path)
            .await
            .map_err(|e| format!("StartDiscovery failed: {e}"))?;
        if !client.is_discovering(&adapter.path).await.unwrap_or(false) {
            return Err("StartDiscovery returned OK but the adapter is not discovering".into());
        }
        let before: Vec<String> = devices.iter().map(|d| d.path.clone()).collect();
        tokio::time::sleep(Duration::from_secs(scan_secs)).await;
        let after = client.devices().await.map_err(|e| e.to_string())?;
        client.stop_discovery(&adapter.path).await.map_err(|e| e.to_string())?;
        let fresh: Vec<_> = after.iter().filter(|d| !before.contains(&d.path)).collect();
        println!("scan     {} devices in range, {} newly seen", after.len(), fresh.len());
        for d in &after {
            if d.rssi.is_some() {
                let tag = if fresh.iter().any(|f| f.path == d.path) { "NEW " } else { "    " };
                println!("  {tag}{:<24} {} rssi={:?} paired={}", d.name, d.address, d.rssi, d.paired);
            }
        }
        if client.is_discovering(&adapter.path).await.unwrap_or(true) {
            return Err("StopDiscovery left the adapter scanning".into());
        }
        println!("scan     stopped cleanly");
    }

    // 6. Real pairing, if asked.
    if let Some(addr) = pair_target {
        let path = qos_bluez::path_for_address(&adapter.path, &addr);
        println!("pair     pairing {addr} at {path}");
        let (agent3, mut events3) = PairingAgent::new(client.names());
        let reg3 = AgentRegistration::register_at(
            client.connection(),
            agent3,
            "/org/qos/BluetoothAgentPair",
            "KeyboardDisplay",
        )
        .await
        .map_err(|e| e.to_string())?;
        tokio::spawn(async move {
            while let Some(ev) = events3.recv().await {
                match ev {
                    AgentEvent::Ask { request, reply } => {
                        println!("\n  BlueZ asks about {}: {:?}", request.device_name, request.prompt);
                        if let Some(t) = request.prompt.passkey_text() {
                            println!("  passkey/PIN: {t}");
                        }
                        if !request.prompt.expects_answer() {
                            continue;
                        }
                        print!("  answer ([y]es / [n]o / digits): ");
                        let _ = io::stdout().flush();
                        let mut line = String::new();
                        let _ = io::stdin().lock().read_line(&mut line);
                        let line = line.trim().to_string();
                        let answer = if line.eq_ignore_ascii_case("y") {
                            PairingReply::Accept
                        } else if let Ok(n) = line.parse::<u32>() {
                            if request.prompt.expects_entry() {
                                PairingReply::Passkey(n)
                            } else {
                                PairingReply::Accept
                            }
                        } else if request.prompt.expects_entry() && !line.is_empty() {
                            PairingReply::PinCode(line)
                        } else {
                            PairingReply::Reject
                        };
                        let _ = reply.send(answer);
                    }
                    AgentEvent::Cancelled => println!("  BlueZ cancelled the request"),
                    AgentEvent::Released => println!("  BlueZ released the agent"),
                }
            }
        });
        let outcome = client
            .pair_and_connect(&adapter.path, &path)
            .await
            .map_err(|e| format!("pairing failed: {}", qos_bluez::error_text(&e)))?;
        println!("pair     {outcome:?}");
        let after = client.devices().await.map_err(|e| e.to_string())?;
        let d = after
            .iter()
            .find(|d| d.path == path)
            .ok_or("the paired device is not in the device list")?;
        if !d.paired {
            return Err(format!("{} reports Paired=false after pairing", d.name));
        }
        if !d.trusted {
            return Err(format!("{} reports Trusted=false after pairing", d.name));
        }
        println!(
            "pair     {} paired={} trusted={} connected={}",
            d.name, d.paired, d.trusted, d.connected
        );
        reg3.unregister().await.map_err(|e| e.to_string())?;
    }

    reg.unregister()
        .await
        .map_err(|e| format!("could not unregister the agent: {e}"))?;
    println!("agent    unregistered cleanly");
    println!("\nOK");
    Ok(())
}
