//! `tilo-secret-service serve` runs the store on the session bus;
//! `status` and `apps …` are its command line, for Settings and scripts.

use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tilo_secret_service::migrate::{self, GnomeKeyring, Outcome};
use tilo_secret_service::service::{
    AppRow, BUS_NAME, BusResolver, Core, MANAGEMENT_PATH, Managers, publish,
};
use tilo_secret_service::store::{DeviceIdentityKey, Store};
use tilo_user_identity::persist::IdentityPaths;
use tracing::{error, info};

#[derive(Parser)]
#[command(about = "Tilo's Secret Service")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve org.freedesktop.secrets on the session bus.
    Serve {
        /// Where the device identity lives instead of `/var/lib/tilo`, and
        /// where one is created when absent. Only development builds have
        /// this: a release build reads the machine's one identity.
        #[cfg(debug_assertions)]
        #[arg(long, hide = true)]
        identity_root: Option<std::path::PathBuf>,
    },
    /// Whether the store is open, and what it imported.
    Status,
    /// The applications that use the store.
    Apps {
        #[command(subcommand)]
        action: Apps,
    },
}

#[derive(Subcommand)]
enum Apps {
    /// List them, one per line or as JSON.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Let an application use the store again.
    Allow { id: String },
    /// Stop an application using the store. Its items stay.
    Revoke { id: String },
    /// Remove an application, its items and its record.
    Forget { id: String },
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let outcome = match cli.command {
        #[cfg(debug_assertions)]
        Command::Serve { identity_root } => serve(identity_root).await,
        #[cfg(not(debug_assertions))]
        Command::Serve {} => serve(None).await,
        Command::Status => status().await,
        Command::Apps { action } => apps(action).await,
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            error!("{message}");
            eprintln!("tilo-secret-service: {message}");
            ExitCode::FAILURE
        }
    }
}

fn uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions.
    unsafe { libc::geteuid() }
}

async fn serve(identity_root: Option<std::path::PathBuf>) -> Result<(), String> {
    let dir = tilo_secret_service::data_dir().ok_or("no home directory")?;
    let uid = uid();
    let paths = match identity_root {
        Some(root) => {
            let paths = IdentityPaths::under(root);
            tilo_user_identity::persist::load_or_create_device_key(&paths)
                .map_err(|e| format!("development identity: {e}"))?;
            paths
        }
        None => IdentityPaths::default_system(),
    };
    let keys = DeviceIdentityKey::new(paths, dir.join("salt"), &uid.to_string());
    let store = Arc::new(Store::new(dir.join("vault.tseal"), Box::new(keys)));
    match store.locked() {
        None => info!("the store is open"),
        Some(why) => info!("the store is locked until the device identity is available: {why}"),
    }

    let connection = zbus::Connection::session()
        .await
        .map_err(|e| format!("session bus: {e}"))?;
    let core = Core::new(
        Arc::clone(&store),
        Arc::new(BusResolver::new(connection.clone(), uid)),
        Managers::system(),
    );
    publish(&connection, &core)
        .await
        .map_err(|e| format!("publishing objects: {e}"))?;

    // Before the name is ours: GNOME Keyring may still hold it.
    match migrate::migrate_once(&store, &GnomeKeyring::from_environment()).await {
        Outcome::Imported(count) => info!("imported {count} items from the older keyring"),
        Outcome::AlreadyDone => {}
        Outcome::Deferred(reason) => info!("the older keyring is not imported yet: {reason}"),
    }

    connection
        .request_name(BUS_NAME)
        .await
        .map_err(|e| format!("{BUS_NAME} is held by another provider: {e}"))?;
    info!("serving {BUS_NAME}");
    tokio::spawn(forget_departed_sessions(
        connection.clone(),
        Arc::clone(&core),
    ));

    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| e.to_string())?;
    tokio::select! {
        _ = terminate.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(())
}

/// A session belongs to the bus name that opened it; it ends with it.
async fn forget_departed_sessions(connection: zbus::Connection, core: Arc<Core>) {
    use futures_util::StreamExt as _;
    let Ok(bus) = zbus::fdo::DBusProxy::new(&connection).await else {
        return;
    };
    let Ok(mut changes) = bus.receive_name_owner_changed().await else {
        return;
    };
    while let Some(change) = changes.next().await {
        let Ok(args) = change.args() else { continue };
        if args.new_owner().is_none() && args.name().as_str().starts_with(':') {
            core.drop_sessions_of(args.name().as_str());
        }
    }
}

async fn management() -> Result<zbus::Proxy<'static>, String> {
    let connection = zbus::Connection::session()
        .await
        .map_err(|e| format!("session bus: {e}"))?;
    zbus::Proxy::new(&connection, BUS_NAME, MANAGEMENT_PATH, "so.tilo.Secrets1")
        .await
        .map_err(|e| e.to_string())
}

async fn status() -> Result<(), String> {
    let proxy = management().await?;
    let (locked, reason, migrated): (bool, String, String) =
        proxy.call("Status", &()).await.map_err(|e| e.to_string())?;
    if locked {
        println!("locked: {reason}");
    } else {
        println!("open");
    }
    if !migrated.is_empty() {
        println!("imported from: {migrated}");
    }
    Ok(())
}

async fn apps(action: Apps) -> Result<(), String> {
    let proxy = management().await?;
    let call_error = |e: zbus::Error| e.to_string();
    match action {
        Apps::List { json } => {
            let rows: Vec<AppRow> = proxy.call("ListApps", &()).await.map_err(call_error)?;
            if json {
                let list: Vec<_> = rows
                    .iter()
                    .map(|(id, name, access, first, last, uses, items)| {
                        serde_json::json!({
                            "id": id, "name": name, "access": access,
                            "first_seen": first, "last_used": last, "uses": uses, "items": items,
                        })
                    })
                    .collect();
                println!("{}", serde_json::Value::Array(list));
            } else {
                for (id, name, access, _, last, uses, items) in rows {
                    println!(
                        "{id}\t{name}\t{access}\tlast used {last}\t{uses} uses\t{items} items"
                    );
                }
            }
        }
        Apps::Allow { id } => proxy
            .call::<_, _, ()>("SetAppAccess", &(id, true))
            .await
            .map_err(call_error)?,
        Apps::Revoke { id } => proxy
            .call::<_, _, ()>("SetAppAccess", &(id, false))
            .await
            .map_err(call_error)?,
        Apps::Forget { id } => proxy
            .call::<_, _, ()>("ForgetApp", &(id,))
            .await
            .map_err(call_error)?,
    }
    Ok(())
}
