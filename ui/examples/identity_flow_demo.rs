//! `identity_flow_demo` — eyeball the Phase 8 identity ceremony.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p ui --example identity_flow_demo
//! cargo run -p ui --example identity_flow_demo -- --screen recovery_share
//! cargo run -p ui --example identity_flow_demo -- --screen invite_mint
//! ```
//!
//! Without `--screen` the example boots on screen 1 (`create_identity`)
//! and the state machine advances through 2 / 2b on submit. With
//! `--screen <name>` the example jumps straight to a specific screen
//! pre-populated with sample data — useful for inspecting the
//! recovery-share grid or the invite-mint URL without walking the
//! ceremony from the top.

use std::sync::Arc;
use std::time::SystemTime;

use ui::widget::identity_flow::{
    IdentityFlow, IdentityFlowScreen, IdentityStore, InviteEnvelope, MemoryIdentityStore,
    mint_invite,
};
use ui::{App, State};
use qos_collab_types::{ChannelId, Role, generate_keypair};
use qos_threshold_identity::ThresholdIdentity;
use rand_core::OsRng;

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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Parse `--screen <name>`; default to CreateIdentity.
    let screen = args
        .windows(2)
        .find(|w| w[0] == "--screen")
        .and_then(|w| parse_screen_arg(&w[1]))
        .unwrap_or(IdentityFlowScreen::CreateIdentity);

    let store: Arc<dyn IdentityStore> = Arc::new(MemoryIdentityStore::new());
    let mut flow = IdentityFlow::new(store.clone());
    flow.set_screen(screen);

    // Seed sample data so each screen has something to render in
    // isolation.
    let mut rng = OsRng;
    let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
    let record = ui::widget::identity_flow::IdentityRecord::from_threshold_identity(
        &id,
        "alice",
        [248, 113, 113],
        SystemTime::now(),
    )
    .unwrap();
    // Recovery seed.
    flow.recovery_share.seed_from_record(&record);
    let mnemonic = flow.recovery_share.mnemonic().to_string();
    flow.recovery_verify.seed_mnemonic(mnemonic);
    // Add-device host screen.
    {
        let (sess, r1) = {
            let mut id_local = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
            id_local.begin_enrolment(1, 2, 2, &mut rng).unwrap()
        };
        let _ = flow
            .add_device_host
            .begin(r1, sess, *id.group_public_key());
    }
    // Invite-mint screen.
    flow.invite_mint.set_channel(ChannelId::new(), "engineering");
    // Invite-accept screen.
    let (owner_sk, owner_pk) = generate_keypair();
    let env: InviteEnvelope = mint_invite(
        ChannelId::new(),
        "engineering",
        owner_pk,
        &owner_sk,
        "Alice",
        Role::Editor,
        SystemTime::now() + std::time::Duration::from_secs(3600),
    )
    .unwrap();
    flow.invite_accept.set_url(env.to_url().unwrap());

    eprintln!(
        "identity_flow_demo: starting on screen {:?}",
        flow.current_screen()
    );

    let state = State::new();
    let app = App::new(state, Box::new(flow));
    app.run();
}
