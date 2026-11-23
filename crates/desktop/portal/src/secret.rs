//! Per-application master-secret portal.
//!
//! The secrets are kept by the person's Secret Service
//! (`tilo-secret-service`), the same store libsecret clients use, sealed
//! under the device identity. This backend only vouches for who is asking:
//! the portal frontend authenticates the app, the session names it, and
//! the store records and may revoke that application like any other. The value never enters workspace state,
//! diagnostics, or a D-Bus reply.

use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::pin::Pin;
use std::sync::Arc;

use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedFd, OwnedValue};
use zeroize::{Zeroize, Zeroizing};

use crate::Portal;

const SECRET_BYTES: usize = 32;
const MAX_TOKEN: usize = 512;
/// Where `tilo-secret-service` serves, and its management object.
const SECRET_SERVICE: &str = "org.freedesktop.secrets";
const SECRET_SERVICE_MANAGEMENT: &str = "/so/tilo/Secrets1";

type Answer<'a> =
    Pin<Box<dyn Future<Output = Result<Zeroizing<[u8; SECRET_BYTES]>, String>> + Send + 'a>>;

/// Where application master secrets come from.
pub trait SandboxSecrets: Send + Sync {
    /// The master secret of the sandboxed app `app`.
    fn secret_for<'a>(&'a self, app: &'a str) -> Answer<'a>;
}

/// The person's Secret Service, asked over the session bus. Only the
/// session may ask it for these; the service checks.
pub struct ServiceSecrets {
    connection: zbus::Connection,
}
impl SandboxSecrets for ServiceSecrets {
    fn secret_for<'a>(&'a self, app: &'a str) -> Answer<'a> {
        Box::pin(async move {
            let service = zbus::Proxy::new(
                &self.connection,
                SECRET_SERVICE,
                SECRET_SERVICE_MANAGEMENT,
                "so.tilo.Secrets1",
            )
            .await
            .map_err(|error| error.to_string())?;
            let request = (app,);
            let ask = || service.call::<_, _, Vec<u8>>("SandboxSecret", &request);
            // The compositor holds the device key and so is non-dumpable:
            // the store cannot read its executable and refuses it until this
            // connection proves the device identity (once per connection,
            // and again after the store restarts).
            let bytes = match ask().await {
                Err(zbus::Error::MethodError(name, _, _))
                    if name.as_str() == "org.freedesktop.Secret.Error.AccessDenied"
                        && tilo_secrets::prove_device_caller(&self.connection)
                            .await
                            .unwrap_or(false) =>
                {
                    ask().await
                }
                answer => answer,
            }
            .map_err(|error| error.to_string())?;
            let bytes = Zeroizing::new(bytes);
            let mut secret = Zeroizing::new([0_u8; SECRET_BYTES]);
            if bytes.len() != secret.len() {
                return Err("the secret store returned a secret of the wrong length".into());
            }
            secret.copy_from_slice(&bytes);
            Ok(secret)
        })
    }
}

/// Write `app`'s secret into `fd` and nowhere else: the response code.
async fn deliver(secrets: &dyn SandboxSecrets, app: &str, fd: std::os::fd::OwnedFd) -> u32 {
    let mut secret = match secrets.secret_for(app).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, app, "the secret store refused an app's secret");
            return 2;
        }
    };
    let result = std::fs::File::from(fd).write_all(secret.as_ref());
    secret.zeroize();
    if result.is_ok() { 0 } else { 2 }
}

/// The `org.freedesktop.impl.portal.Secret` interface.
#[derive(Clone)]
pub struct Secret {
    portal: Portal,
    secrets: Arc<dyn SandboxSecrets>,
}

impl Secret {
    /// Secrets from `store`.
    #[must_use]
    pub fn new(portal: Portal, store: Arc<dyn SandboxSecrets>) -> Self {
        Self {
            portal,
            secrets: store,
        }
    }

    /// Secrets of the person's Secret Service on `connection`.
    #[must_use]
    pub fn on_session(portal: Portal, connection: zbus::Connection) -> Self {
        Self::new(portal, Arc::new(ServiceSecrets { connection }))
    }
}

#[interface(name = "org.freedesktop.impl.portal.Secret")]
impl Secret {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }

    async fn retrieve_secret(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        app_id: String,
        fd: OwnedFd,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let Ok(auth) = self
            .portal
            .authenticate(&header, handle.as_str(), &app_id, None)
            .await
        else {
            return (2, HashMap::new());
        };
        if crate::option::<String>(&options, "token")
            .is_some_and(|token| !valid_identifier(&token, MAX_TOKEN))
        {
            return (2, HashMap::new());
        }
        let fd: std::os::fd::OwnedFd = fd.into();
        (
            deliver(&*self.secrets, &auth.caller.app_id, fd).await,
            HashMap::new(),
        )
    }
}

fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.contains('\0') && !value.contains('/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// A store that mints a secret per app, or refuses every app.
    struct Fake {
        refuse: bool,
    }

    impl SandboxSecrets for Fake {
        fn secret_for<'a>(&'a self, app: &'a str) -> Answer<'a> {
            Box::pin(async move {
                if self.refuse {
                    return Err("revoked".into());
                }
                let mut out = Zeroizing::new([0_u8; SECRET_BYTES]);
                out.fill(app.len().try_into().unwrap());
                Ok(out)
            })
        }
    }

    fn pipe() -> (std::io::PipeReader, std::os::fd::OwnedFd) {
        let (read, write) = std::io::pipe().unwrap();
        (read, write.into())
    }

    #[tokio::test]
    async fn the_secret_reaches_only_the_descriptor_the_app_passed() {
        let (mut read, write) = pipe();
        assert_eq!(
            deliver(&Fake { refuse: false }, "so.tilo.notes", write).await,
            0
        );
        let mut got = Vec::new();
        read.read_to_end(&mut got).unwrap();
        assert_eq!(got, vec![13_u8; SECRET_BYTES]);
    }

    #[tokio::test]
    async fn a_revoked_app_gets_no_secret() {
        let (mut read, write) = pipe();
        assert_eq!(
            deliver(&Fake { refuse: true }, "so.tilo.notes", write).await,
            2
        );
        let mut got = Vec::new();
        read.read_to_end(&mut got).unwrap();
        assert!(got.is_empty());
    }
}
