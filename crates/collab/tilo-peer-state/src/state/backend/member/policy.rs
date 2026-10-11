//! Current policy copied from the admitted room, within explicit limits.
use super::{Error, GovernedIdentity, PeerLimits, Policy, Role, key_text};
use crate::resources::json::{self, JsonLimits};
use crate::room::{
    policy::{self, Destination},
    read::{ReadError, RoomRead},
};
use std::collections::BTreeMap;
use tilo_action_policy::Grant;

pub(super) struct NativePolicy {
    owner: Option<String>,
    members: BTreeMap<String, Role>,
    grants: Vec<Grant>,
    identity: GovernedIdentity,
}
impl Policy for NativePolicy {
    fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }
    fn members(&self) -> &BTreeMap<String, Role> {
        &self.members
    }
    fn grants(&self) -> &[Grant] {
        &self.grants
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
}
struct Collect<'a> {
    owner: Option<String>,
    members: BTreeMap<String, Role>,
    grants: Vec<Grant>,
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
    fn grant(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let parsed: Grant = match json::decode(bytes, self.limits) {
            Ok(value) => value,
            Err(crate::resources::PeerResourceError::Malformed) => return Ok(()),
            Err(error) => return Err(Error::Decode(error)),
        };
        // The decoded grant holds at most what its encoding spelt out.
        self.output.pay(bytes.len())?;
        self.grants.push(parsed);
        Ok(())
    }
    fn member(&mut self, key: &str, bytes: &[u8]) -> Result<(), Error> {
        let role: Role = match json::decode(bytes, self.limits) {
            Ok(value) => value,
            Err(crate::resources::PeerResourceError::Malformed) => return Ok(()),
            Err(error) => return Err(Error::Decode(error)),
        };
        let key = self.output.text(key)?;
        self.members.insert(key, role);
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
        fn grant(&mut self, _: &[u8]) -> Result<(), Error> {
            self.0 = self.0.checked_add(1).ok_or(Error::Capacity)?;
            Ok(())
        }
        fn member(&mut self, _: &str, _: &[u8]) -> Result<(), Error> {
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
            .checked_mul(size_of::<Grant>())
            .ok_or(Error::Capacity)?,
    )?;
    let mut grants = Vec::new();
    grants
        .try_reserve_exact(count.0)
        .map_err(|_| Error::Capacity)?;
    let key = key_text(source);
    let key = key.as_str();
    let mut collect = Collect {
        owner: None,
        members: BTreeMap::new(),
        grants,
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
        members: collect.members,
        grants: collect.grants,
        identity,
    })
}
