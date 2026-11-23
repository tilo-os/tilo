//! Who each connection to the native endpoint is, and the device's answers about it: which
//! account signed in as which person (the actions `seats#signed-in` and `seats#left`, root's
//! alone to call), and who a connection is (`{"native": "account#who"}`).
//!
//! The account a connection comes from is the kernel's account of its peer ([`peer_uid`]), and
//! the daemon's admission ([`tilo_api::daemon::admission`]) says what it acts as. Which person an
//! account is, root says when the account signs in, and the daemon never guesses it from the uid:
//! it keeps that person for the account until root says the account left. What root hands over
//! is read from the account's identity directory, so it comes with the person's local person
//! proof ([`tilo_identity::local_person`]), signed by that person for this device's key and this
//! account: an account cannot name a person it is not.

use std::io;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use serde_json::{Value, json};
use tilo_actions::remote::{
    CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueResource, CatalogueRow,
};
use tilo_actions::{ActionAddress, ActionParam, ParamType};
use tilo_api::daemon::admission::{Admission, Admitted, ROOT};

use crate::device::unclaimed::DeviceOnly;

/// The resource the seats' actions are on, and the app they are listed as.
const SEATS: &str = "seats";
/// The provider the device answers the seats' actions as.
pub(crate) const PROVIDER: &str = "tilo.seats";
/// What the seats' catalogue is.
const REVISION: &str = "tilo.seats/1";
/// Say an account signed in: `{person, uid, proof}`, the person's key and its proof as hex.
const SIGNED_IN: &str = "signed-in";
/// Say an account's session ended: `{uid}`.
const LEFT: &str = "left";
/// Who this connection is: `{person?, device, label?, claimed}`.
const WHO: &str = "account#who";
/// Why anyone but root is refused the seats' actions.
pub(crate) const ROOT_ALONE: &str = "only root says who signed in on this computer, or who left";

/// The seats as the device lists them beside its own actions: a provider of their own.
pub(crate) fn provider() -> CatalogueProvider {
    CatalogueProvider {
        provider: PROVIDER.to_owned(),
        app_id: SEATS.to_owned(),
        workspace: String::new(),
        browsable: true,
        ..CatalogueProvider::default()
    }
}

/// The page `query` asks of the seats' actions.
pub(crate) fn catalogue(query: &CatalogueQuery) -> CataloguePage {
    CataloguePage::from_rows(REVISION.to_owned(), rows(), query)
}

/// The seats' actions.
fn rows() -> Vec<CatalogueRow> {
    let resource = CatalogueResource {
        id: SEATS.to_owned(),
        kind: "seat".to_owned(),
        label: "Seats".to_owned(),
        presentation: None,
    };
    let row = |action| CatalogueRow {
        provider: PROVIDER.to_owned(),
        app_id: SEATS.to_owned(),
        parent_resource: String::new(),
        resource: resource.clone(),
        action,
        ..CatalogueRow::default()
    };
    let uid = || ActionParam::required("uid", ParamType::Integer, "The account's uid.");
    let action = |name: &str, label: &str, description: &str, params: &[ActionParam]| {
        let mut descriptor = crate::device::ticket::descriptor(name, label, description, params);
        descriptor.address = ActionAddress::new(SEATS, name);
        "seat".clone_into(&mut descriptor.resource_kind);
        "Seats".clone_into(&mut descriptor.resource_label);
        Some(descriptor)
    };
    vec![
        row(None),
        row(action(
            SIGNED_IN,
            "An account signed in",
            "Serve the account as its person until it leaves. Root's alone to say.",
            &[
                ActionParam::required("person", ParamType::Text, "The person's key, as hex."),
                uid(),
                ActionParam::required(
                    "proof",
                    ParamType::Text,
                    "The account's local person proof, as hex.",
                ),
            ],
        )),
        row(action(
            LEFT,
            "An account left",
            "Serve the account the device's own actions alone again. Root's alone to say.",
            &[uid()],
        )),
    ]
}

/// How a connection's account is read: the kernel's account of its peer, or a test's stand-in.
pub(crate) type UidOf = fn(&UnixStream) -> io::Result<u32>;

/// The account at the other end of `stream`, as the kernel says.
///
/// # Errors
/// The kernel does not say.
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        Ok(rustix::net::sockopt::socket_peercred(stream)?.uid.as_raw())
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        use std::os::fd::AsRawFd as _;
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: getpeereid writes the two ids it is handed and reads nothing else.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
}

/// One connection's account, and what it is admitted as.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Seat {
    pub(crate) uid: u32,
    pub(crate) admitted: Admitted,
}

/// The daemon's seats: its admission, and the device it answers for.
#[derive(Clone)]
pub(crate) struct Seats {
    admission: &'static Admission,
    device: DeviceOnly,
    uid_of: UidOf,
}

impl Seats {
    pub(crate) fn new(admission: &'static Admission, device: DeviceOnly, uid_of: UidOf) -> Self {
        Self {
            admission,
            device,
            uid_of,
        }
    }

    /// Who the connection `stream` is. One whose account the kernel does not say is a stranger.
    pub(crate) fn seat(&self, stream: &UnixStream) -> Seat {
        match (self.uid_of)(stream) {
            Ok(uid) => Seat {
                uid,
                admitted: self.admission.admit(uid),
            },
            Err(error) => {
                tracing::debug!(%error, "a native connection's account is not known");
                Seat {
                    uid: u32::MAX,
                    admitted: Admitted::Stranger,
                }
            }
        }
    }

    /// Who the connection is, when `request` asks (`{"native": "account#who"}`); `None` for any
    /// other request.
    pub(crate) fn answer(&self, seat: Seat, request: &Value) -> Option<Value> {
        (request.get("native").and_then(Value::as_str) == Some(WHO)).then(|| self.who(seat))
    }

    /// The seat action `action` with `args`, as the account `seat` asks it: root's alone.
    fn act(&self, seat: Seat, action: &str, args: &Value) -> Result<Value, String> {
        if seat.uid != ROOT {
            return Err(ROOT_ALONE.to_owned());
        }
        match action {
            SIGNED_IN => self.signed_in(seat, args),
            LEFT => uid_in(args).and_then(|uid| {
                self.admission
                    .left(seat.uid, uid)
                    .map(|was| json!({"left": was}))
            }),
            other => Err(format!("seats has no action {other}")),
        }
    }

    fn signed_in(&self, seat: Seat, request: &Value) -> Result<Value, String> {
        let uid = uid_in(request)?;
        let hex_in = |field: &str| {
            request
                .get(field)
                .and_then(Value::as_str)
                .and_then(|value| hex::decode(value).ok())
        };
        let person = hex_in("person")
            .and_then(|person| <[u8; 32]>::try_from(person).ok())
            .ok_or("a sign-in names the person's key, as hex")?;
        let proof =
            hex_in("proof").ok_or("a sign-in names the account's local person proof, as hex")?;
        self.person_proven(person, uid, &proof)?;
        self.admission.signed_in(seat.uid, uid, person)?;
        Ok(json!({"person": hex::encode(person)}))
    }

    /// Whether `person` is the one this device serves, and `proof` theirs for the account `uid`.
    fn person_proven(&self, person: [u8; 32], uid: u32, proof: &[u8]) -> Result<(), String> {
        let (bound, device) = tilo_identity::local_person::device_binding(self.device.keys())
            .map_err(|error| format!("this device is bound to no person: {error}"))?;
        if device != self.device.public_key() {
            return Err("this device's certificate names another key than its own".to_owned());
        }
        if person != bound {
            return Err("this device serves another person".to_owned());
        }
        if !tilo_identity::local_person::proves(&person, &device, uid, proof) {
            return Err(format!(
                "account {uid} holds no proof that it is this person"
            ));
        }
        Ok(())
    }

    fn who(&self, seat: Seat) -> Value {
        let person = match seat.admitted {
            Admitted::Person(person) => Some(person),
            // A person's own service is that person's; the system's daemon and root are none.
            Admitted::Device if !self.admission.system() => self.device.person(),
            Admitted::Device | Admitted::Stranger => None,
        };
        // The person and the label only where there is one: a computer nobody set up is its
        // device alone.
        let mut who = serde_json::Map::new();
        if let Some(person) = person {
            who.insert("person".into(), json!(hex::encode(person)));
        }
        who.insert(
            "device".into(),
            json!(hex::encode(self.device.public_key())),
        );
        if let Some(label) = self.device.label() {
            who.insert("label".into(), json!(label));
        }
        who.insert("claimed".into(), json!(self.device.claimed()));
        Value::Object(who)
    }
}

/// The account a seats request names.
fn uid_in(request: &Value) -> Result<u32, String> {
    request
        .get("uid")
        .and_then(Value::as_u64)
        .and_then(|uid| u32::try_from(uid).ok())
        .ok_or_else(|| "a seats request names the account by its uid".to_owned())
}

/// A connection's seat and the daemon's seats, as a native connection is served with them.
#[derive(Clone)]
pub(crate) struct Seated {
    pub(crate) seat: Seat,
    pub(crate) seats: Arc<Seats>,
}

impl Seated {
    /// The seat action `action` with `args`, as this connection asks it.
    pub(crate) fn act(&self, action: &str, args: &Value) -> Result<Value, String> {
        self.seats.act(self.seat, action, args)
    }
}
