//! Default native Signal assembly: one signed sandbox, one RAM lease and
//! separately authenticated streams. No ambient daemon or unsigned fallback.
use super::{Arc, NativeSignalAccount, NativeSignalHandles, SendError, assemble_native};
use launcher::owned_service::{ServiceCancellation, ServiceEndpoint, ServiceLaunch};
use std::{
    io,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

/// Original native host-selected storage; no application chooses these paths.
pub struct NativeSignalStorage {
    pub profile: PathBuf,
    pub history: PathBuf,
    pub runtime: PathBuf,
    pub signal_account: String,
}
pub(super) fn installed_launch() -> Result<ServiceLaunch, SendError> {
    let installed = package_store::find_installed(super::working_profile::SERVICE_PACKAGE)
        .map_err(|_| SendError::Unavailable)?
        .ok_or(SendError::Unavailable)?;
    Ok(ServiceLaunch {
        manifest: installed.manifest_path,
        payload: installed.payload_root,
        options: launcher::run::RunOptions {
            app_id: Some(super::working_profile::SERVICE_PACKAGE.into()),
            expected_package_hash: Some(installed.package_id_hex),

            mode: launcher::run::LaunchMode::Service {
                passthrough_argv: vec![],
            },
            no_services: true,
            identity: launcher::run::IdentitySource::None,
            ..launcher::run::RunOptions::default()
        },
    })
}
pub(super) fn acquire_launch(
    check: impl Fn() -> Result<(), SendError>,
    cancellation: &ServiceCancellation,
) -> Result<ServiceLaunch, SendError> {
    check()?;
    if cancellation.is_cancelled() {
        return Err(SendError::Revoked);
    }
    if package_store::find_installed(super::working_profile::SERVICE_PACKAGE)
        .map_err(|_| SendError::Unavailable)?
        .is_none()
    {
        crate::shell::catalogue::install_service_package(super::working_profile::SERVICE_PACKAGE)
            .map_err(|_| SendError::Unavailable)?;
    }
    check()?;
    if cancellation.is_cancelled() {
        return Err(SendError::Revoked);
    }
    installed_launch()
}

pub(super) fn streams(
    endpoint: &ServiceEndpoint,
) -> io::Result<[std::os::unix::net::UnixStream; 2]> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(20))
        .ok_or(io::ErrorKind::InvalidData)?;
    let first = loop {
        match endpoint.connect() {
            Ok(stream) => break stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    };
    Ok([first, endpoint.connect()?])
}
impl NativeSignalAccount {
    pub(super) fn activate_default_on_worker(
        self: &Arc<Self>,
        wake: crate::session_runtime::Wake,
        storage: NativeSignalStorage,
        policy: super::network::Policy,
    ) -> Result<super::NativeSignalActivation, SendError> {
        if [&storage.profile, &storage.history, &storage.runtime]
            .iter()
            .any(|path| !path.is_absolute())
        {
            return Err(SendError::InvalidRequest);
        }
        let account = self.clone();
        self.activate_on_worker(wake, move |authority| {
            let cancellation = policy.cancellation.clone();
            let consent = super::network::request(account.gate.startup.identity.clone(), &policy)?;
            authority.check_account_scope(&format!("signal:{}", storage.signal_account))?;
            let launch = acquire_launch(|| account.gate.check(), &cancellation)?;
            let profile = account.materialize_profile(&storage.profile, &storage.runtime)?;
            let (send, receive) = mpsc::sync_channel(1);
            let service = cancellation.clone();
            let runtime = crate::session_runtime::runtime().map_err(|_| SendError::Unavailable)?;
            drop(runtime.spawn_blocking(move || {
                let result = profile.run_connected_and_checkpoint(
                    launch,
                    service,
                    Some(consent),
                    Some(Box::new(move |endpoint| {
                        let pair = streams(&endpoint)?;
                        send.send(pair)
                            .map_err(|_| io::ErrorKind::Interrupted.into())
                    })),
                );
                if result.is_err() {
                    tracing::debug!("private Signal service stopped without a clean checkpoint");
                }
            }));
            let pair = receive.recv_timeout(Duration::from_secs(25)).map_err(|_| {
                cancellation.cancel();
                SendError::Unavailable
            })?;
            let [sender, receiver] = pair;
            let result = assemble_native(
                &account,
                NativeSignalHandles {
                    private_root: storage.history,
                    signal_account: storage.signal_account,
                    sender,
                    receiver,
                },
                authority,
            );
            if result.is_err() {
                cancellation.cancel();
            }
            result
        })
    }
}
impl super::super::InProcessApps {
    /// Invoke the native signed provider factory on ordinary session workers.
    /// This performs no enrollment or permission grant. The original host
    /// retains cancellation; dropping it stops only its own private service.
    /// # Errors
    /// Refuses missing approved custody, installed provider or native wake.
    pub fn start_default_native_signal_account(
        &mut self,
        startup: super::SignalAccountStartup,
        storage: NativeSignalStorage,
    ) -> Result<Arc<NativeSignalAccount>, ui::launch::LaunchError> {
        let cancellation = ServiceCancellation::for_owner(Arc::downgrade(&self.messaging_lifetime));
        let policy = super::network::Policy {
            cancellation: cancellation.clone(),
            prompts: self.messaging_network_prompts.clone(),
        };
        let account = self.start_owned_signal_account(startup, |account, wake| {
            account.activate_default_on_worker(wake, storage, policy)
        })?;
        self.messaging_providers.push(cancellation);
        self.messaging_native_accounts.push(account.clone());
        Ok(account)
    }
}

impl super::super::InProcessApps {
    /// Ordinary empty-app startup invokes this factory for existing, explicitly
    /// granted native accounts. Editable metadata alone never starts a service.
    pub(in crate::shell::in_process) fn prepare_default_messaging(&mut self) {
        let Some(root) = self.native_messaging_root.clone() else {
            return;
        };
        let Ok(accounts) = self.default_native_accounts("signal") else {
            return;
        };
        if accounts.is_empty() {
            return;
        }
        let Some(vault) = self.credential_owner.as_ref() else {
            return;
        };
        let Ok(grants) = vault.grants() else {
            return;
        };
        for (id, row) in accounts {
            if self.messaging_native_accounts.iter().any(|account| {
                account.owner().service == tilo_messaging_protocol::Service::Signal
                    && account.owner().account == id
            }) {
                continue;
            }
            if self
                .messaging_services
                .iter()
                .any(|(identity, _)| identity.service == "signal" && identity.account == id)
            {
                continue;
            }
            let Some((generation, scope, aci)) = signal_identity(&row) else {
                continue;
            };
            let owner = super::Owner {
                principal: self.author.0.clone(),
                device: self.device_id.clone(),
                service: tilo_messaging_protocol::Service::Signal,
                account: id,
                linking_generation: generation,
            };
            let exact = super::credential_scope(&owner);
            let Some(grant) = grants.iter().find(|grant| {
                grant.app_id == "so.tilo.messages" && grant.scope.as_ref() == Some(&exact)
            }) else {
                continue;
            };
            let Ok(identity) = self.native_messaging_identity(owner.clone()) else {
                continue;
            };
            let request = accounts::AccessRequest {
                app_id: "so.tilo.messages".into(),
                item_id: grant.item_id.clone(),
                purpose: "Private Signal messaging".into(),
                origin: grant.origin.clone(),
            };
            let Ok(credential) = self.native_messaging_credential(&identity, request) else {
                continue;
            };
            let Ok(authority) = self.native_messaging_authority(&identity, &credential, &scope)
            else {
                continue;
            };
            let Some(runtime) = tilo_paths::xdg::runtime_dir() else {
                continue;
            };
            // No raw account/device labels become filesystem components.
            let Ok(domain) = serde_json::to_vec(&owner) else {
                continue;
            };
            let private = root.join(blake3::hash(&domain).to_hex().as_str());
            let storage = NativeSignalStorage {
                profile: private.join("profile"),
                history: private.join("history"),
                runtime,
                signal_account: aci,
            };
            let _ = self.start_default_native_signal_account(
                super::SignalAccountStartup {
                    identity,
                    credential,
                    authority,
                },
                storage,
            );
        }
    }
    pub(super) fn default_native_accounts(
        &mut self,
        service: &str,
    ) -> Result<Vec<(String, accounts::person::PersonAccount)>, SendError> {
        use accounts::person::PersonAccounts;
        use tilo_resource::Resource as _;
        let records = ui::record::person::PersonRecords::open(
            &mut self.owners,
            accounts::person::ACCOUNTS_OWNER,
            self.author.clone(),
        )
        .map_err(|_| SendError::Unavailable)?;
        let record = records
            .open_original_record(ui::record::RecordKind::new(PersonAccounts::KIND), |doc| {
                PersonAccounts::initialize(doc).map_err(|error| error.to_string())
            })
            .map_err(|_| SendError::Unavailable)?;
        let state = record
            .read(PersonAccounts::read)
            .ok_or(SendError::Unavailable)?
            .map_err(|_| SendError::Unavailable)?;
        Ok(state
            .accounts
            .iter()
            .filter(|(_, row)| row.enabled && row.type_id == service)
            .map(|(id, row)| (id.as_str().to_owned(), row.clone()))
            .collect())
    }
}

fn signal_identity(row: &accounts::person::PersonAccount) -> Option<(u64, String, String)> {
    let generation = row
        .config
        .get("linking_generation")?
        .as_u64()
        .filter(|value| *value > 0)?;
    let scope = row.config.get("messaging_scope")?.as_str()?;
    let aci = scope.strip_prefix("signal:")?;
    if uuid::Uuid::parse_str(aci).is_ok_and(|parsed| parsed.to_string() == aci) {
        Some((generation, scope.into(), aci.into()))
    } else {
        None
    }
}
