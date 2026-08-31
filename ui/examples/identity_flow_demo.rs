//! `identity_flow_demo` — eyeball the Phase 8 identity ceremony.
//!
//! Run with:
//!
//! ```bash
//! # Volatile demo, in-memory only:
//! cargo run -p ui --example ui-lab -- identity_flow_demo
//!
//! # Persist to a directory (the demo writes identity.json on success):
//! cargo run -p ui --example ui-lab -- identity_flow_demo --persist /tmp/qos-alice
//!
//! # Jump to a specific screen pre-seeded with sample data:
//! cargo run -p ui --example ui-lab -- identity_flow_demo --screen recovery_share
//! ```
//!
//! With `--persist <DIR>` the demo saves the created identity to
//! `<DIR>/identity.json` (mode 0600). A second instance of the demo
//! pointed at a different `--persist` path creates a distinct identity
//! the same way — two windows side-by-side simulates the "I'm Alice
//! on this laptop, you're Bob on yours" first-boot story.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use collab_types::{ChannelId, Role, generate_keypair};
use rand_core::OsRng;
use threshold_identity::ThresholdIdentity;
use ui::widget::identity_flow::{
    IdentityFlow, IdentityFlowError, IdentityFlowScreen, IdentityRecord, IdentityStore,
    InviteEnvelope, MemoryIdentityStore, mint_invite,
};
use ui::{App, State};

fn parse_screen_arg(name: &str) -> Option<IdentityFlowScreen> {
    Some(match name {
        "create_identity" => IdentityFlowScreen::CreateIdentity,
        "recovery_share" => IdentityFlowScreen::RecoveryShare,
        "recovery_verify" => IdentityFlowScreen::RecoveryVerify,
        "add_device_host" => IdentityFlowScreen::AddDeviceHost,
        "pair_device" => IdentityFlowScreen::PairDevice,
        "invite_mint" => IdentityFlowScreen::InviteMint,
        "invite_accept" => IdentityFlowScreen::InviteAccept,
        _ => return None,
    })
}

/// Disk-backed `IdentityStore` for the demo. Writes JSON at the
/// given path with mode 0600. Mirrors `shell::identity_provisioning::
/// DiskIdentityStore` minus the XDG dir-resolution boilerplate.
pub struct DemoFileStore {
    path: PathBuf,
    cached: Mutex<Option<IdentityRecord>>,
}

impl DemoFileStore {
    pub fn at(path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                let mut perm = fs::metadata(parent)?.permissions();
                perm.set_mode(0o700);
                let _ = fs::set_permissions(parent, perm);
            }
        }
        let cached = if path.exists() {
            let bytes = fs::read(&path)?;
            IdentityRecord::from_json_bytes(&bytes).ok()
        } else {
            None
        };
        Ok(Self {
            path,
            cached: Mutex::new(cached),
        })
    }
}

impl IdentityStore for DemoFileStore {
    fn load(&self) -> Result<Option<IdentityRecord>, IdentityFlowError> {
        let cached = self
            .cached
            .lock()
            .map_err(|err| IdentityFlowError::Io(io::Error::other(err.to_string())))?;
        Ok(cached.clone())
    }
    fn save(&self, record: &IdentityRecord) -> Result<(), IdentityFlowError> {
        let bytes = record.to_json_bytes()?;
        // 0600 lands on the temp file, so the record is never readable by
        // anyone else at its final path.
        atomic_file::write_with_mode(&self.path, &bytes, 0o600).map_err(IdentityFlowError::Io)?;
        {
            let mut cached = self
                .cached
                .lock()
                .map_err(|err| IdentityFlowError::Io(io::Error::other(err.to_string())))?;
            *cached = Some(record.clone());
        }
        eprintln!(
            "identity_flow_demo: wrote identity to {} (0600)",
            self.path.display()
        );
        Ok(())
    }
}

pub fn run(args: &[String]) {
    let screen = args
        .windows(2)
        .find(|w| w.first().is_some_and(|flag| flag == "--screen"))
        .and_then(|w| w.get(1).and_then(|value| parse_screen_arg(value)))
        .unwrap_or(IdentityFlowScreen::CreateIdentity);

    let persist_dir = args
        .windows(2)
        .find(|w| w.first().is_some_and(|flag| flag == "--persist"))
        .and_then(|w| w.get(1).map(PathBuf::from));

    let store: Arc<dyn IdentityStore> = if let Some(dir) = persist_dir {
        let path = dir.join("identity.json");
        eprintln!("identity_flow_demo: persisting to {}", path.display());
        match DemoFileStore::at(path) {
            Ok(store) => Arc::new(store),
            Err(err) => {
                eprintln!("identity_flow_demo: failed to init DemoFileStore: {err}");
                return;
            }
        }
    } else {
        eprintln!("identity_flow_demo: in-memory only (no --persist)");
        Arc::new(MemoryIdentityStore::new())
    };

    let mut flow = IdentityFlow::new(store.clone());
    flow.set_screen(screen);

    if let Err(err) = seed_flow(&mut flow) {
        eprintln!("identity_flow_demo: failed to seed demo data: {err}");
        return;
    }

    eprintln!(
        "identity_flow_demo: starting on screen {:?}",
        flow.current_screen()
    );

    let state = State::new();
    let app = App::new(state, Box::new(flow));
    app.run();
}

/// Seed `flow` with a bootstrapped identity, a recovery share/verify pair,
/// an in-progress device-enrolment round, and a signed invite — everything
/// the showcase screens need pre-populated data to render meaningfully.
///
/// # Errors
///
/// Returns an error if any of the threshold-identity, record, or invite
/// construction steps fail. Never happens with this hardcoded seed data,
/// but each step is fallible in its real signature.
#[allow(
    clippy::similar_names,
    reason = "sk/pk is the standard secret-key/public-key naming convention; \
              distinguishing them further would reduce clarity"
)]
fn seed_flow(flow: &mut IdentityFlow) -> Result<(), IdentityFlowError> {
    let mut rng = OsRng;
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
    let record =
        IdentityRecord::from_threshold_identity(&id, "alice", [248, 113, 113], SystemTime::now())?;
    flow.recovery_share.seed_from_record(&record);
    let mnemonic = flow.recovery_share.mnemonic().to_string();
    flow.recovery_verify.seed_mnemonic(mnemonic);
    {
        let (sess, r1) = {
            let mut id_local = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
            id_local.begin_enrolment(1, 2, 2, &mut rng)?
        };
        let _ = flow.add_device_host.begin(r1, sess, *id.group_public_key());
    }
    flow.invite_mint
        .set_channel(ChannelId::new(), "engineering");
    let (owner_sk, owner_pk) = generate_keypair();
    // checked_add: SystemTime's Add<Duration> panics on overflow. One hour
    // from now never overflows; falling back to "now" keeps this a real
    // handled path rather than a panic if it somehow ever did.
    let expires_at = SystemTime::now()
        .checked_add(std::time::Duration::from_secs(3600))
        .unwrap_or_else(SystemTime::now);
    let env: InviteEnvelope = mint_invite(
        ChannelId::new(),
        "engineering",
        owner_pk,
        &owner_sk,
        "Alice",
        Role::Editor,
        expires_at,
    )?;
    flow.invite_accept.set_url(env.to_url()?);
    Ok(())
}
