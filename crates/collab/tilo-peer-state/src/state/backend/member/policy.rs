//! Current policy copied from the admitted room, within explicit limits.
use super::{
    CapOverride, Error, GovernedIdentity, PeerLimits, Policy, Principal, Scope, Subject, key_text,
};
use crate::resources::json::{self, JsonLimits};
use crate::room::{
    policy::{self, Destination},
    read::{ReadError, RoomRead},
};

pub(super) struct NativePolicy {
    owner: Option<String>,
    caps: Vec<CapOverride>,
    identity: GovernedIdentity,
}
impl NativePolicy {
    pub(super) fn caps_mut(&mut self) -> &mut Vec<CapOverride> {
        &mut self.caps
    }
}
impl Policy for NativePolicy {
    fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }
    fn caps(&self) -> &[CapOverride] {
        &self.caps
    }
    fn identity(&self) -> &GovernedIdentity {
        &self.identity
    }
}
/// Bytes left for the copied policy, from `governance_collection_bytes`.
struct Output {
    remaining: usize,
}
impl Output {
    fn pay(&mut self, bytes: usize) -> Result<(), Error> {
        self.remaining = self.remaining.checked_sub(bytes).ok_or(Error::Capacity)?;
        Ok(())
    }
    fn text(&mut self, value: &str) -> Result<String, Error> {
        self.pay(value.len())?;
        Ok(value.to_owned())
    }
    fn principal(&mut self, value: &Principal) -> Result<Principal, Error> {
        Ok(match value {
            Principal::Person(s) => Principal::Person(self.text(s)?),
            Principal::App(s) => Principal::App(self.text(s)?),
            Principal::Agent(s) => Principal::Agent(self.text(s)?),
            Principal::Token(s) => Principal::Token(self.text(s)?),
            Principal::Room(s) => Principal::Room(self.text(s)?),
        })
    }
    fn capability(&mut self, value: &CapOverride) -> Result<CapOverride, Error> {
        let subject = match &value.subject {
            Subject::Role(role) => Subject::Role(*role),
            Subject::User(key) => Subject::User(*key),
            Subject::Principal(p) => Subject::Principal(self.principal(p)?),
        };
        let scope = match &value.scope {
            Scope::Workspace => Scope::Workspace,
            Scope::ResourceType(kind) => Scope::ResourceType(*kind),
            Scope::ResourceInstance(id) => Scope::ResourceInstance(*id),
            Scope::App(s) => Scope::App(self.text(s)?),
            Scope::FsPath(s) => Scope::FsPath(self.text(s)?),
            Scope::Device(s) => Scope::Device(self.text(s)?),
            Scope::NetHost(s) => Scope::NetHost(self.text(s)?),
            Scope::Action { app, action } => Scope::Action {
                app: self.text(app)?,
                action: self.text(action)?,
            },
            Scope::Room(s) => Scope::Room(self.text(s)?),
            Scope::FsRecord(s) => Scope::FsRecord(self.text(s)?),
        };
        Ok(CapOverride {
            subject,
            scope,
            effect: value.effect,
            verb: value.verb,
            id: self.text(&value.id)?,
            expiry: value.expiry,
            granted_by: value
                .granted_by
                .as_ref()
                .map(|p| self.principal(p))
                .transpose()?,
        })
    }
}
struct Collect<'a> {
    owner: Option<String>,
    caps: Vec<CapOverride>,
    identity: Option<GovernedIdentity>,
    key: &'a str,
    limits: JsonLimits,
    output: Output,
}
impl Destination for Collect<'_> {
    type Error = Error;
    fn owner(&mut self, value: &str) -> Result<(), Error> {
        self.owner = Some(self.output.text(value)?);
        Ok(())
    }
    fn capability(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let parsed: CapOverride = match json::decode(bytes, self.limits) {
            Ok(value) => value,
            Err(crate::resources::PeerResourceError::Malformed) => return Ok(()),
            Err(error) => return Err(Error::Decode(error)),
        };
        let cap = self.output.capability(&parsed)?;
        self.caps.push(cap);
        Ok(())
    }
    fn identity(&mut self, key: &str, bytes: &[u8]) -> Result<(), Error> {
        if key == self.key {
            self.identity = Some(json::decode(bytes, self.limits).map_err(Error::Decode)?);
        }
        Ok(())
    }
}
fn read_error(error: ReadError<Error>) -> Error {
    match error {
        ReadError::Malformed => Error::Read,
        ReadError::Capacity => Error::Capacity,
        ReadError::Visitor(e) => e,
    }
}
pub(super) fn collect<R: RoomRead>(
    read: &R,
    source: &[u8; 32],
    limits: PeerLimits,
) -> Result<NativePolicy, Error> {
    struct Count(usize);
    impl Destination for Count {
        type Error = Error;
        fn owner(&mut self, _: &str) -> Result<(), Error> {
            Ok(())
        }
        fn identity(&mut self, _: &str, _: &[u8]) -> Result<(), Error> {
            Ok(())
        }
        fn capability(&mut self, _: &[u8]) -> Result<(), Error> {
            self.0 = self.0.checked_add(1).ok_or(Error::Capacity)?;
            Ok(())
        }
    }
    let mut count = Count(0);
    policy::project(read, &mut count).map_err(read_error)?;
    let mut output = Output {
        remaining: limits.governance_collection_bytes,
    };
    output.pay(
        count
            .0
            .checked_mul(size_of::<CapOverride>())
            .ok_or(Error::Capacity)?,
    )?;
    let mut caps = Vec::new();
    caps.try_reserve_exact(count.0)
        .map_err(|_| Error::Capacity)?;
    let key = key_text(source);
    let key = key.as_str();
    let mut collect = Collect {
        owner: None,
        caps,
        identity: None,
        key,
        limits: JsonLimits {
            allocation_bytes: limits.governance_json.allocation_bytes,
            collection_entries: limits.governance_json.collection_entries,
        },
        output,
    };
    policy::project(read, &mut collect).map_err(read_error)?;
    let identity = collect.identity.ok_or(Error::Absent)?;
    Ok(NativePolicy {
        owner: collect.owner,
        caps: collect.caps,
        identity,
    })
}
