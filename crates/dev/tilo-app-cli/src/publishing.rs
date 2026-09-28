//! `tilo-app keys`, `tilo-app apply` and `tilo-app publish`: a publisher key,
//! the namespaces it may publish under, and the builds it publishes.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Subcommand;
use registry_client::{RegistryClient, key};
use registry_types::application::{
    NamespaceClaim, dns_txt_name, forge_proof_url, proof_token, well_known_url,
};
use registry_types::namespace::Forge;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub struct KeysArgs {
    #[command(subcommand)]
    command: KeysCommand,
}

#[derive(Subcommand)]
enum KeysCommand {
    /// Create a publisher key. An existing key is never replaced.
    New {
        /// Where to write it; defaults to `$XDG_CONFIG_HOME/tilo/publisher-key.json`.
        #[arg(long)]
        key: Option<PathBuf>,
    },
    /// Print the key's public half and the line that proves a namespace.
    Show {
        /// The key file; defaults to `$TILO_PUBLISHER_KEY`, then the default path.
        #[arg(long)]
        key: Option<PathBuf>,
    },
}

#[derive(clap::Args)]
pub struct ApplyArgs {
    /// A domain you control; proves the namespace of its reversed labels
    /// (`example.com` proves `com.example`). Repeatable.
    #[arg(long = "domain")]
    domains: Vec<String>,
    /// A git forge account, as `github:NAME`, `codeberg:NAME` or
    /// `HOST:NAME` for a Forgejo or Gitea host. Repeatable.
    #[arg(long = "forge")]
    forges: Vec<String>,
    /// How the registry's owner can reach you.
    #[arg(long, required_unless_present = "status")]
    contact: Option<String>,
    /// Anything the owner should know.
    #[arg(long, default_value = "")]
    note: String,
    /// Only report the status of this key's application.
    #[arg(long)]
    status: bool,
    #[arg(long)]
    key: Option<PathBuf>,
    /// The registry; defaults to `$TILO_REGISTRY`, then `https://apps.tilo.so`.
    #[arg(long)]
    registry: Option<String>,
}

#[derive(clap::Args)]
pub struct PublishArgs {
    /// Packages to publish [default: every dist/*.tapp].
    packages: Vec<PathBuf>,
    /// Store presentation for a new entry: name, tagline, description, images.
    #[arg(long)]
    metadata: Option<PathBuf>,
    /// The architecture of a Tilo OS build whose manifest declares several.
    #[arg(long)]
    arch: Option<String>,
    #[arg(long)]
    key: Option<PathBuf>,
    /// The registry; defaults to `$TILO_REGISTRY`, then `https://apps.tilo.so`.
    #[arg(long)]
    registry: Option<String>,
}

pub fn run_keys(args: KeysArgs) -> Result<Value> {
    let key = match args.command {
        KeysCommand::New { key: path } => {
            let path = path
                .or_else(key::default_path)
                .ok_or_else(|| anyhow!("no configuration directory; pass --key"))?;
            let created = key::create(&path)?;
            eprintln!(
                "[tilo-app] wrote {}; back it up, it is the only copy",
                path.display()
            );
            created
        }
        KeysCommand::Show { key: path } => key::resolve(path.as_deref())?,
    };
    let public = key::public_hex(&key);
    let token = proof_token(&public);
    println!("{public}");
    eprintln!("[tilo-app] to prove a namespace, publish this line where only you can write:");
    eprintln!("  {token}");
    Ok(json!({"public_key": public, "proof": token}))
}

pub fn run_apply(args: ApplyArgs) -> Result<Value> {
    let key = key::resolve(args.key.as_deref())?;
    let client = RegistryClient::resolve(args.registry.as_deref())?;
    let public = key::public_hex(&key);
    if args.status {
        let status = client
            .application(&public)?
            .ok_or_else(|| anyhow!("{} has no application from this key", client.base()))?;
        println!("{}", status_line(&status));
        return Ok(status);
    }
    let mut claims: Vec<NamespaceClaim> = args
        .domains
        .into_iter()
        .map(|domain| NamespaceClaim::Domain { domain })
        .collect();
    for forge in &args.forges {
        claims.push(forge_claim(forge)?);
    }
    if claims.is_empty() {
        bail!("name a namespace to prove with --domain or --forge");
    }
    let token = proof_token(&public);
    eprintln!("[tilo-app] the registry looks for `{token}` at:");
    for claim in &claims {
        for place in proof_places(claim) {
            eprintln!("  {place}");
        }
    }
    let contact = args.contact.unwrap_or_default();
    let status = registry_client::apply(&client, &key, claims, contact, args.note)?;
    println!("{}", status_line(&status));
    eprintln!(
        "[tilo-app] the registry's owner reviews it; `tilo-app apply --status` reports the decision"
    );
    Ok(status)
}

pub fn run_publish(args: PublishArgs) -> Result<Value> {
    let key = key::resolve(args.key.as_deref())?;
    let client = RegistryClient::resolve(args.registry.as_deref())?;
    let packages = if args.packages.is_empty() {
        dist_packages(&std::env::current_dir()?.join("dist"))?
    } else {
        args.packages
    };
    let metadata = args
        .metadata
        .map(|path| {
            let text = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            serde_json::from_slice::<registry_client::EntryMetadata>(&text)
                .with_context(|| format!("parse {}", path.display()))
        })
        .transpose()?;
    let bytes = packages
        .iter()
        .map(|package| {
            std::fs::read(package).with_context(|| format!("read {}", package.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    let outcomes = registry_client::publish(
        &client,
        &key,
        &bytes,
        args.arch.as_deref(),
        metadata.as_ref(),
    )?;
    let mut published = Vec::new();
    for (package, outcome) in packages.iter().zip(outcomes) {
        let action = outcome.action.map_or("unchanged", |action| match action {
            registry_types::EntryAction::Add => "add",
            registry_types::EntryAction::Update => "update",
            registry_types::EntryAction::Delete => "delete",
        });
        eprintln!(
            "[tilo-app] {} {} ({}) uploaded; entry {action} awaits review at {}",
            outcome.package_id,
            outcome.version,
            outcome.platform,
            client.base()
        );
        published.push(json!({
            "package": package,
            "package_id": outcome.package_id,
            "version": outcome.version,
            "platform": outcome.platform,
            "url": outcome.url,
            "entry": action,
        }));
    }
    Ok(json!({"registry": client.base(), "published": published}))
}

fn dist_packages(dist: &Path) -> Result<Vec<PathBuf>> {
    let mut packages: Vec<PathBuf> = std::fs::read_dir(dist)
        .with_context(|| format!("read {}; run `tilo-app package` first", dist.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "tapp")
        })
        .collect();
    packages.sort();
    if packages.is_empty() {
        bail!(
            "{} has no .tapp; run `tilo-app package` first",
            dist.display()
        );
    }
    Ok(packages)
}

fn forge_claim(text: &str) -> Result<NamespaceClaim> {
    let (forge, account) = text
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("--forge {text:?}: expected FORGE:ACCOUNT, such as github:ada"))?;
    let parsed = Forge::parse(forge).ok_or_else(|| anyhow!("--forge {text:?}: unknown forge"))?;
    if parsed.namespace(account).is_none() {
        bail!("--forge {text:?}: {account:?} is not an account name");
    }
    Ok(NamespaceClaim::Forge {
        forge: forge.to_owned(),
        account: account.to_owned(),
    })
}

fn proof_places(claim: &NamespaceClaim) -> Vec<String> {
    match claim {
        NamespaceClaim::Domain { domain } => vec![
            format!("DNS TXT {}", dns_txt_name(domain)),
            format!("or {}", well_known_url(domain)),
        ],
        NamespaceClaim::Forge { forge, account } => Forge::parse(forge)
            .map(|forge| vec![forge_proof_url(&forge, account)])
            .unwrap_or_default(),
    }
}

fn status_line(status: &Value) -> String {
    let namespaces = status
        .get("namespaces")
        .and_then(Value::as_array)
        .map(|namespaces| {
            namespaces
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let state = status
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let reason = status
        .get("reason")
        .and_then(Value::as_str)
        .map(|reason| format!(": {reason}"))
        .unwrap_or_default();
    format!("{state} ({namespaces}){reason}")
}
