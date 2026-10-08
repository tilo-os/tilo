//! Qualify an accepted installed Signal provider with an empty private profile.
//! No signer, account, credential, network grant or alternate launcher is used.
use launcher::owned_service::{
    ServiceAuthority, ServiceCancellation, ServiceEndpoint, ServiceLaunch, ServiceLeaseKey,
};
use launcher::run::{IdentitySource, LaunchMode, RunOptions};
use std::error::Error;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const PACKAGE: &str = "so.tilo.signal-provider";

// Fixture admission applies only to a newly created, empty private data home.
// The normal launcher still verifies the actual installation and runtime trust.
struct EmptyProfileAdmission(Arc<AtomicBool>);
impl ServiceAuthority for EmptyProfileAdmission {
    fn check(&self) -> io::Result<()> {
        if self.0.load(Ordering::Acquire) {
            Err(io::ErrorKind::PermissionDenied.into())
        } else {
            Ok(())
        }
    }
}

fn inspect_empty(endpoint: &ServiceEndpoint) -> io::Result<()> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(20))
        .ok_or(io::ErrorKind::InvalidData)?;
    let mut stream = loop {
        match endpoint.connect() {
            Ok(stream) => break stream,
            Err(error) if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    };
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"fixture-empty\",\"method\":\"listAccounts\",\"params\":{}}\n")?;
    let mut reply = Vec::new();
    BufReader::new(stream)
        .take(65537)
        .read_until(b'\n', &mut reply)?;
    if reply.len() > 65536 || !reply.ends_with(b"\n") {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let value: serde_json::Value = serde_json::from_slice(&reply)?;
    if value.get("id").and_then(serde_json::Value::as_str) != Some("fixture-empty")
        || value.get("result") != Some(&serde_json::json!([]))
    {
        // Never print a response which unexpectedly contains an account.
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let expected_version = std::env::args()
        .nth(1)
        .ok_or("exact accepted version required")?;
    if std::env::args().len() != 2 {
        return Err("only the exact accepted version may be supplied".into());
    }
    let installed = package_store::find_installed(PACKAGE)?
        .ok_or("accepted Signal provider is not installed")?;
    if installed.version != expected_version {
        return Err("installed provider differs from the accepted release".into());
    }
    let root = tempfile::tempdir()?;
    let data_home = root.path().to_path_buf();
    let cancellation = ServiceCancellation::default();
    let revoked = Arc::new(AtomicBool::new(false));
    let authority = Arc::new(EmptyProfileAdmission(revoked.clone()));
    let callback_revoke = revoked;
    let key = ServiceLeaseKey::default();
    let launch = ServiceLaunch {
        manifest: installed.manifest_path,
        payload: installed.payload_root,
        options: RunOptions {
            app_id: Some(PACKAGE.into()),
            expected_package_hash: Some(installed.package_id_hex),
            data_home: Some(data_home.clone()),
            mode: LaunchMode::Service {
                passthrough_argv: vec![],
            },
            no_services: true,
            identity: IdentitySource::None,
            ..RunOptions::default()
        },
    };
    let result = launcher::owned_service::run_connected(
        launch,
        authority,
        cancellation,
        key.clone(),
        Some(Box::new(move |endpoint| {
            inspect_empty(&endpoint)?;
            callback_revoke.store(true, Ordering::Release);
            match endpoint.connect() {
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(()),
                _ => Err(io::ErrorKind::InvalidData.into()),
            }
        })),
    );
    let exit = match result {
        Ok(exit) => exit,
        Err(error) => {
            if std::env::var_os(launcher::run::DEBUG_STDERR_ENV).as_deref()
                == Some(std::ffi::OsStr::new("1"))
            {
                eprintln!(
                    "empty-profile diagnostic directory: {}",
                    root.keep().display()
                );
            }
            return Err(error.into());
        }
    };
    if !exit.matches(&key, &data_home, PACKAGE) {
        return Err("original owned namespace did not finish".into());
    }
    println!(
        "accepted installed Signal provider: verified namespace peer, empty account list, admission revocation and quiescent shutdown passed"
    );
    Ok(())
}
