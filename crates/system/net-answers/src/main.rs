//! `tilo-net-answers <source>`: fetch one network answer for search and
//! print it as JSON on stdout (see the library). It confines itself first:
//! it reads only the trust bundle and the resolver's files, writes nothing,
//! and connects only to HTTPS.

use std::process::ExitCode;
use std::time::Duration;

use tilo_net_answers::Source;

/// Longest a fetch may take; the shell gives up on the helper a little later.
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Largest body read; reference rates are a few kilobytes.
const MAX_BODY_BYTES: u64 = 1024 * 1024;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(name), None) = (args.next(), args.next()) else {
        eprintln!("usage: tilo-net-answers rates");
        return ExitCode::from(2);
    };
    let Some(source) = Source::named(&name) else {
        eprintln!("unknown answer source {name:?}");
        return ExitCode::from(2);
    };
    match run(source) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn run(source: Source) -> Result<String, String> {
    warm_resolver(source.url());
    confine()?;
    let client = tilo_net::http::blocking_client()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|error| format!("no HTTP client: {error}"))?;
    let response = client
        .get(source.url())
        .send()
        .map_err(|error| format!("could not fetch {}: {error}", source.name()))?;
    if !response.status().is_success() {
        return Err(format!(
            "could not fetch {}: the server answered {}",
            source.name(),
            response.status()
        ));
    }
    let mut body = String::new();
    std::io::Read::read_to_string(
        &mut std::io::Read::take(response, MAX_BODY_BYTES),
        &mut body,
    )
    .map_err(|error| format!("could not read {}: {error}", source.name()))?;
    let answer = source.parse(&body)?;
    serde_json::to_string(&answer).map_err(|error| error.to_string())
}

#[cfg(not(target_os = "linux"))]
fn run(source: Source) -> Result<String, String> {
    Err(format!("{} is fetched only on Linux", source.name()))
}

/// Resolve `url`'s host once while unconfined. The first lookup loads the
/// C library's name-service modules from the system library directory,
/// which the confined helper may no longer read; loaded, they serve the
/// fetch's own lookup. A failure here is the fetch's to report.
#[cfg(target_os = "linux")]
fn warm_resolver(url: &str) {
    if let Some(host) = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
    {
        let _ = std::net::ToSocketAddrs::to_socket_addrs(&(host, 443));
    }
}

/// Read the trust bundle and the resolver's files, write nothing, connect
/// only to port 443; refuse to run unconfined.
#[cfg(target_os = "linux")]
fn confine() -> Result<(), String> {
    use sandbox_core::landlock;
    let readable: Vec<std::path::PathBuf> = [
        "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/gai.conf",
        "/etc/host.conf",
        "/run/systemd/resolve",
    ]
    .into_iter()
    .map(std::path::PathBuf::from)
    .filter(|path| path.exists())
    .collect();
    let rules = landlock::access_plan(&readable, &[], landlock::TARGET_ABI);
    let policy =
        landlock::RulesetSpec::application(rules, landlock::NetRestriction::ConnectOnly(443));
    let status = landlock::restrict_self(&policy, |_, _| {})
        .map_err(|error| format!("could not confine the helper: {error}"))?;
    if landlock::outcome_from_status(&status.ruleset) != landlock::LandlockOutcome::FullyEnforced {
        return Err("the kernel cannot confine the helper; nothing is fetched".into());
    }
    Ok(())
}
