//! `tilo settings`: read or change a profile's settings from a script.
//!
//! While the session holds the settings, a change goes to the session, which
//! makes it.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use settings_store::SettingsSource as _;

#[derive(Subcommand)]
pub enum SettingsCmd {
    /// The whole settings document, as JSON.
    Dump {
        /// The profile's directory instead of the person's.
        #[arg(long)]
        profile: Option<PathBuf>,
    },
    /// Merge a JSON object into one section.
    Set {
        section: String,
        json: String,
        /// The profile's directory instead of the person's.
        #[arg(long)]
        profile: Option<PathBuf>,
    },
}

fn open(profile: Option<PathBuf>) -> Result<std::sync::Arc<settings_store::Store>> {
    let root = match profile {
        Some(root) => root,
        None => settings_store::config_root().context("no config directory")?,
    };
    Ok(settings_store::Store::open(&root)?)
}

pub fn run(cmd: SettingsCmd) -> Result<()> {
    match cmd {
        SettingsCmd::Dump { profile } => {
            let document = serde_json::Value::Object(open(profile)?.load());
            println!("{}", serde_json::to_string_pretty(&document)?);
        }
        SettingsCmd::Set {
            section,
            json,
            profile,
        } => {
            let serde_json::Value::Object(values) = serde_json::from_str(&json)? else {
                bail!("the value must be a JSON object");
            };
            let store = open(profile)?;
            store.update_section(&section, values)?;
            store.flush_pending();
        }
    }
    Ok(())
}
