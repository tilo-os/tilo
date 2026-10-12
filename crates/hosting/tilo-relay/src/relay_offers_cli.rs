//! `tilo-relay-offers` — ask this device's relays what their workers run
//! for environments, and record the answers for Terminal's relay picker.
//!
//! ```text
//! tilo-relay-offers --identity-root <dir> --offers <file> [--budget-ms 2000]
//! ```
//!
//! Every relay is asked at once; whichever have not answered within the
//! budget keep what they said before. The `ush` supervisor starts this and
//! does not wait for it.

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "tilo-relay-offers",
    about = "Record what this device's relays run for environments"
)]
struct Args {
    /// Where this device's identity and home-peer registrations live.
    #[arg(long, default_value = "/var/lib/tilo")]
    identity_root: PathBuf,
    /// The offers file to update.
    #[arg(long)]
    offers: PathBuf,
    /// How long all the relays together may take, milliseconds.
    #[arg(long, default_value_t = 2000)]
    budget_ms: u64,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let offers = tilo_relay::relay_offers::refresh_offers(
        &args.identity_root,
        &args.offers,
        Duration::from_millis(args.budget_ms),
        tilo_clock::unix_now_ms(),
    )
    .await?;
    println!("{}", serde_json::to_string(&offers)?);
    Ok(())
}
