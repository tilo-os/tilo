//! Retained current policy copied from the exact native room read.
use super::*;
use crate::resources::json::{JsonHeld, JsonLimits};
use crate::room::{
    policy::{self, Destination},
    read::{ReadError, RoomRead},
};

pub(super) struct NativePolicy {
    owner: Option<String>,
    caps: Vec<CapOverride>,
    identity: JsonHeld<GovernedIdentity>,
    _retention: NativeReservation,
}
impl Policy for NativePolicy {
    fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }
    fn caps(&self) -> &[CapOverride] {
        &self.caps
    }
    fn identity(&self) -> &GovernedIdentity {
        self.identity.value()
    }
    fn role(
        &self,
        person: &str,
        now: u64,
        turn: Option<&NativeWorkTurn>,
    ) -> Result<Option<Role>, Error> {
        // role_of's only owned temporary is hex::encode([u8;32]) for a User
        // subject: one exact 64-byte Vec, reused/freed between rows.
        let _scratch = turn
            .expect("native policy turn")
            .reserve(64)
            .map_err(Error::Resource)?;
        Ok(room::role_of(self.owner(), self.caps(), person, now))
    }
}
struct Output {
    remaining: usize,
    retention: NativeReservation,
}
impl Output {
    fn pay(&mut self, bytes: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or(Error::Resource(NativeResourceError::WorkBytes))?;
        Ok(())
    }
    fn text(&mut self, value: &str) -> Result<String, Error> {
        self.pay(value.len())?;
        let mut copy = String::new();
        copy.try_reserve_exact(value.len())
            .map_err(|_| Error::Resource(NativeResourceError::WorkBytes))?;
        if copy.capacity() > value.len() {
            return Err(Error::Resource(NativeResourceError::Unaccounted));
        }
        copy.push_str(value);
        Ok(copy)
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
    identity: Option<JsonHeld<GovernedIdentity>>,
    key: &'a str,
    turn: &'a NativeWorkTurn,
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
        let parsed = match JsonHeld::<CapOverride>::decode(bytes, self.limits, self.turn) {
            Ok(value) => value,
            Err(crate::resources::PeerResourceError::Malformed) => return Ok(()),
            Err(error) => return Err(Error::Decode(error)),
        };
        let cap = self.output.capability(parsed.value())?;
        // Counted before construction. No geometric growth is permitted.
        if self.caps.len() == self.caps.capacity() {
            return Err(Error::Resource(NativeResourceError::Unaccounted));
        }
        self.caps.push(cap);
        Ok(())
    }
    fn identity(&mut self, key: &str, bytes: &[u8]) -> Result<(), Error> {
        if key == self.key {
            self.identity =
                Some(JsonHeld::decode(bytes, self.limits, self.turn).map_err(Error::Decode)?)
        }
        Ok(())
    }
}
fn read_error(error: ReadError<Error>) -> Error {
    match error {
        ReadError::Malformed => Error::Read,
        ReadError::Resource(e) => Error::Resource(e),
        ReadError::Visitor(e) => e,
    }
}
pub(super) fn collect<R: RoomRead>(
    read: &R,
    source: &[u8; 32],
    turn: &NativeWorkTurn,
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
            self.0 = self
                .0
                .checked_add(1)
                .ok_or(Error::Resource(NativeResourceError::WorkBytes))?;
            Ok(())
        }
    }
    let mut count = Count(0);
    policy::project(read, &mut count).map_err(read_error)?;
    let mut output = Output {
        remaining: limits.governance_collection_bytes,
        retention: turn
            .reserve(limits.governance_collection_bytes)
            .map_err(Error::Resource)?,
    };
    output.pay(
        count
            .0
            .checked_mul(size_of::<CapOverride>())
            .ok_or(Error::Resource(NativeResourceError::WorkBytes))?,
    )?;
    let mut caps = Vec::new();
    caps.try_reserve_exact(count.0)
        .map_err(|_| Error::Resource(NativeResourceError::WorkBytes))?;
    if caps.capacity() > count.0 {
        return Err(Error::Resource(NativeResourceError::Unaccounted));
    }
    let key = key_text(source);
    let key = std::str::from_utf8(&key).expect("hex key");
    let mut collect = Collect {
        owner: None,
        caps,
        identity: None,
        key,
        turn,
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
        _retention: collect.output.retention,
    })
}
