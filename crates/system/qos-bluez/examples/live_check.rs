//! Live check against the running `bluetoothd` on this machine.
//!
//! This is a tool, not a test: it is deliberately outside the test suite so
//! it can insist on observing real hardware instead of skipping when the bus
//! is absent. Every failure — no system bus, no adapter, an agent `BlueZ`
//! refuses to register, a decline that does not come back as a decline —
//! exits non-zero.
//!
//! ```text
//! cargo run -p qos-bluez --example live_check                 # adapter + agent round trip
//! cargo run -p qos-bluez --example live_check -- --scan 15    # ...and scan for 15s
//! cargo run -p qos-bluez --example live_check -- --pair AA:BB:CC:DD:EE:FF
//! ```
//!
//! `--pair` runs the real pairing flow and answers the agent from the
//! terminal, which is what a compositor prompt does with a pointer.
//!
//! # Why part of this runs as root
//!
//! Fedora's `/usr/share/dbus-1/system.d/bluetooth.conf` grants
//! `send_interface="org.bluez.Agent1"` to the **root** policy only. The one
//! thing on the machine allowed to invoke a pairing agent is therefore
//! `bluetoothd` itself. That refusal is the defence — it is what stops any
//! user process from driving the pairing dialog — so the check exercises the
//! agent through the same gate by re-executing itself under `sudo` to play
//! the daemon's part, and separately asserts that an unprivileged caller is
//! still turned away.

use std::io::{self, BufRead, Write};
use std::time::Duration;

use qos_bluez::agent::{AgentEvent, AgentRegistration, PairingAgent};
use qos_bluez::model::{PairingPrompt, PairingReply};
use qos_bluez::{AdapterInfo, AgentEvents, BluezClient, DeviceInfo};

/// The device path the simulated calls name. It does not have to exist —
/// the agent is being asked a question about it, not asked to reach it.
const DEV: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF";

#[tokio::main]
async fn main() {
    // Re-executed under sudo to stand in for bluetoothd.
    if let Some(target) = arg_value("--as-bluez") {
        std::process::exit(match bluez_side(&target).await {
            Ok(line) => {
                println!("{line}");
                0
            }
            Err(e) => {
                eprintln!("RESULT=FAIL {e}");
                1
            }
        });
    }
    if let Err(e) = run().await {
        eprintln!("\nFAIL: {e}");
        std::process::exit(1);
    }
}

fn arg_value(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i.saturating_add(1)).cloned())
}

/// The root half: call `org.bluez.Agent1.RequestConfirmation` on the agent
/// exactly as the daemon would, and report the outcome — including the D-Bus
/// **error name**, which is the part that matters and which `busctl` does not
/// print.
async fn bluez_side(unique_name: &str) -> Result<String, String> {
    let path = arg_value("--path").ok_or("--as-bluez needs --path")?;
    let passkey: u32 = arg_value("--passkey")
        .ok_or("--as-bluez needs --passkey")?
        .parse()
        .map_err(|_| "--passkey wants a number".to_string())?;
    let conn = zbus::Connection::system()
        .await
        .map_err(|e| format!("root system-bus connection failed: {e}"))?;
    let proxy = zbus::Proxy::new(&conn, unique_name, path, "org.bluez.Agent1")
        .await
        .map_err(|e| format!("cannot address the agent: {e}"))?;
    let device = zbus::zvariant::ObjectPath::try_from(DEV).map_err(|e| e.to_string())?;
    match proxy
        .call_method("RequestConfirmation", &(&device, passkey))
        .await
    {
        Ok(_) => Ok("RESULT=OK".to_string()),
        Err(zbus::Error::MethodError(name, detail, _)) => Ok(format!(
            "RESULT=ERROR name={} detail={}",
            name.as_str(),
            detail.unwrap_or_default()
        )),
        Err(e) => Err(e.to_string()),
    }
}

/// Ask the root half to make the call. Failing to become root is a hard
/// error: the check must never quietly degrade into not exercising the agent.
async fn as_bluez(unique_name: &str, path: &str, passkey: u32) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let unique_name = unique_name.to_string();
    let path = path.to_string();
    // Blocking on the runtime here would starve the task that answers the
    // prompt, and the call would "time out" against our own agent.
    tokio::task::spawn_blocking(move || {
        let out = std::process::Command::new("sudo")
            .args(["-n", &exe.to_string_lossy()])
            .args([
                "--as-bluez",
                &unique_name,
                "--path",
                &path,
                "--passkey",
                &passkey.to_string(),
            ])
            .output()
            .map_err(|e| format!("cannot re-exec under sudo to stand in for bluetoothd: {e}"))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let line = text
            .lines()
            .find(|l| l.starts_with("RESULT="))
            .ok_or_else(|| format!("no result from the root half: {}", text.trim()))?;
        Ok(line.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Step 1: reach `bluetoothd` and report the machine's adapter.
async fn connect_and_show_adapter() -> Result<(BluezClient, AdapterInfo), String> {
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
    Ok((client, adapter))
}

/// Step 2: report every device `bluetoothd` already knows about.
async fn list_devices(client: &BluezClient) -> Result<Vec<DeviceInfo>, String> {
    let devices = client
        .devices()
        .await
        .map_err(|e| format!("cannot list devices: {e}"))?;
    println!("devices  {} known", devices.len());
    for d in &devices {
        println!(
            "  {:<22} {} paired={} trusted={} connected={} rssi={:?}",
            d.name, d.address, d.paired, d.trusted, d.connected, d.rssi
        );
    }
    Ok(devices)
}

/// Step 3: register the pairing agent with the real daemon as the default,
/// so pairings started from anywhere route to it.
async fn register_default_agent(
    client: &BluezClient,
) -> Result<(AgentRegistration, AgentEvents, String), String> {
    let (agent, events) = PairingAgent::new(client.names());
    let reg = AgentRegistration::register(client.connection(), agent)
        .await
        .map_err(|e| format!("bluetoothd refused to register the pairing agent: {e}"))?;
    println!(
        "agent    registered at {} and requested as default",
        reg.path()
    );
    let unique = client
        .connection()
        .unique_name()
        .ok_or("our connection has no unique name")?
        .to_string();
    Ok((reg, events, unique))
}

/// Step 4: a passkey confirmation the user accepts must unblock the caller.
async fn accept_round_trip(mut events: AgentEvents, unique: &str) -> Result<(), String> {
    let accept = tokio::spawn(async move {
        let AgentEvent::Ask { request, reply } =
            events.recv().await.ok_or("agent channel closed")?
        else {
            return Err("expected an Ask event".to_string());
        };
        println!(
            "prompt   \"{}\" asks {:?}, shown as {:?}",
            request.device_name,
            request.prompt,
            request.prompt.passkey_text()
        );
        if request.prompt != (PairingPrompt::ConfirmPasskey { passkey: 424_242 }) {
            return Err(format!("unexpected prompt: {:?}", request.prompt));
        }
        reply
            .send(PairingReply::Accept)
            .map_err(|_| "could not answer".to_string())?;
        Ok::<_, String>(request.device_name)
    });
    let line = as_bluez(unique, qos_bluez::AGENT_PATH, 424_242).await?;
    if line != "RESULT=OK" {
        return Err(format!("an accepted passkey did not succeed: {line}"));
    }
    let named = accept.await.map_err(|e| e.to_string())??;
    println!("agent    accept  -> {line}  (device {named})");
    Ok(())
}

/// Step 5: the bus policy is the gate — the same call from this unprivileged
/// process must be refused before it ever reaches our object.
async fn refuse_unprivileged_caller(unique: &str) -> Result<(), String> {
    let caller = zbus::Connection::system()
        .await
        .map_err(|e| format!("second system-bus connection failed: {e}"))?;
    let proxy = zbus::Proxy::new(&caller, unique, qos_bluez::AGENT_PATH, "org.bluez.Agent1")
        .await
        .map_err(|e| format!("cannot address our own agent object: {e}"))?;
    let device = zbus::zvariant::ObjectPath::try_from(DEV).map_err(|e| e.to_string())?;
    match proxy
        .call_method("RequestConfirmation", &(&device, 1u32))
        .await
    {
        Ok(_) => Err(
            "an unprivileged process drove the pairing agent — the bus policy \
                    restricting org.bluez.Agent1 to root is not in force"
                .to_string(),
        ),
        Err(zbus::Error::MethodError(name, _, _))
            if name.as_str() == "org.freedesktop.DBus.Error.AccessDenied" =>
        {
            println!(
                "agent    unprivileged caller refused by bus policy (only bluetoothd may ask)"
            );
            Ok(())
        }
        Err(e) => Err(format!(
            "unprivileged call failed for the wrong reason: {e}"
        )),
    }
}

/// Step 6: a decline must come back as `org.bluez.Error.Rejected` — that name
/// is what aborts the pairing rather than leaving it hanging. Runs on its own
/// connection: `BlueZ` keys agents by sender and refuses a second
/// `RegisterAgent` from one connection with `AlreadyExists`. Returns the
/// registration so the caller can unregister it once the timeout check below
/// has also run.
async fn decline_round_trip(client: &BluezClient) -> Result<AgentRegistration, String> {
    let conn2 = zbus::Connection::system()
        .await
        .map_err(|e| format!("third system-bus connection failed: {e}"))?;
    let unique2 = conn2
        .unique_name()
        .ok_or("the second connection has no unique name")?
        .to_string();
    let (agent2, mut events2) = PairingAgent::with_timeout(client.names(), Duration::from_secs(10));
    let path2 = "/org/qos/BluetoothAgentRefuseCheck";
    let reg2 = AgentRegistration::register_at(&conn2, agent2, path2, "KeyboardDisplay")
        .await
        .map_err(|e| format!("cannot register the second agent: {e}"))?;
    tokio::spawn(async move {
        if let Some(AgentEvent::Ask { reply, .. }) = events2.recv().await {
            let _ = reply.send(PairingReply::Reject);
        }
    });
    let line = as_bluez(&unique2, path2, 1).await?;
    if !line.contains("name=org.bluez.Error.Rejected") {
        return Err(format!(
            "a declined pairing did not return Rejected: {line}"
        ));
    }
    println!("agent    decline -> {line}");
    Ok(reg2)
}

/// Step 7: nobody answering must come back as `Canceled`, not as a hang and
/// not as a silent success. Returns the registration so the caller can
/// unregister it alongside [`decline_round_trip`]'s.
async fn timeout_round_trip(client: &BluezClient) -> Result<AgentRegistration, String> {
    let (agent3, events3) = PairingAgent::with_timeout(client.names(), Duration::from_secs(2));
    let path3 = "/org/qos/BluetoothAgentTimeoutCheck";
    let conn3 = zbus::Connection::system()
        .await
        .map_err(|e| e.to_string())?;
    let unique3 = conn3.unique_name().ok_or("no unique name")?.to_string();
    let reg3 = AgentRegistration::register_at(&conn3, agent3, path3, "KeyboardDisplay")
        .await
        .map_err(|e| format!("cannot register the third agent: {e}"))?;
    drop(events3); // nothing is listening at all
    let line = as_bluez(&unique3, path3, 7).await?;
    if !line.contains("name=org.bluez.Error.Rejected") {
        return Err(format!("an unattended prompt did not refuse: {line}"));
    }
    println!("agent    no UI   -> {line}");
    Ok(reg3)
}

/// Step 8: real discovery, if `--scan` was given.
async fn run_scan(
    client: &BluezClient,
    adapter: &AdapterInfo,
    devices: &[DeviceInfo],
    scan_secs: u64,
) -> Result<(), String> {
    println!(
        "scan     starting {scan_secs}s discovery on {}",
        adapter.path
    );
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
    client
        .stop_discovery(&adapter.path)
        .await
        .map_err(|e| e.to_string())?;
    let fresh: Vec<_> = after.iter().filter(|d| !before.contains(&d.path)).collect();
    println!(
        "scan     {} devices known, {} newly seen",
        after.len(),
        fresh.len()
    );
    for d in &after {
        if d.rssi.is_some() || fresh.iter().any(|f| f.path == d.path) {
            let tag = if fresh.iter().any(|f| f.path == d.path) {
                "NEW "
            } else {
                "    "
            };
            println!(
                "  {tag}{:<24} {} rssi={:?} paired={}",
                d.name, d.address, d.rssi, d.paired
            );
        }
    }
    if client.is_discovering(&adapter.path).await.unwrap_or(true) {
        return Err("StopDiscovery left the adapter scanning".into());
    }
    println!("scan     stopped cleanly");
    Ok(())
}

/// Step 9: real pairing, if `--pair ADDR` was given. Answers the agent from
/// the terminal, which is what a compositor prompt does with a pointer.
async fn run_pairing(
    client: &BluezClient,
    adapter: &AdapterInfo,
    addr: &str,
) -> Result<(), String> {
    let path = qos_bluez::path_for_address(&adapter.path, addr);
    println!("pair     pairing {addr} at {path}");
    let (agent4, mut events4) = PairingAgent::new(client.names());
    let conn4 = zbus::Connection::system()
        .await
        .map_err(|e| e.to_string())?;
    let reg4 = AgentRegistration::register_at(
        &conn4,
        agent4,
        "/org/qos/BluetoothAgentPair",
        "KeyboardDisplay",
    )
    .await
    .map_err(|e| e.to_string())?;
    tokio::spawn(async move {
        while let Some(ev) = events4.recv().await {
            match ev {
                AgentEvent::Ask { request, reply } => {
                    println!(
                        "\n  BlueZ asks about {}: {:?}",
                        request.device_name, request.prompt
                    );
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
    reg4.unregister().await.map_err(|e| e.to_string())?;
    Ok(())
}

async fn run() -> Result<(), String> {
    let scan_secs: u64 = arg_value("--scan")
        .map(|s| {
            s.parse()
                .map_err(|_| "--scan wants a number of seconds".to_string())
        })
        .transpose()?
        .unwrap_or(0);
    let pair_target = arg_value("--pair");

    let (client, adapter) = connect_and_show_adapter().await?;
    let devices = list_devices(&client).await?;

    let (reg, events, unique) = register_default_agent(&client).await?;
    accept_round_trip(events, &unique).await?;
    refuse_unprivileged_caller(&unique).await?;

    let reg2 = decline_round_trip(&client).await?;
    let reg3 = timeout_round_trip(&client).await?;
    reg3.unregister().await.map_err(|e| e.to_string())?;
    reg2.unregister().await.map_err(|e| e.to_string())?;

    if scan_secs > 0 {
        run_scan(&client, &adapter, &devices, scan_secs).await?;
    }

    if let Some(addr) = pair_target {
        run_pairing(&client, &adapter, &addr).await?;
    }

    reg.unregister()
        .await
        .map_err(|e| format!("could not unregister the agent: {e}"))?;
    println!("agent    unregistered cleanly");
    println!("\nOK");
    Ok(())
}
