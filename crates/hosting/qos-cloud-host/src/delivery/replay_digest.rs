//! Exact inert Cloud replay digest. No owner, admission or policy assertion.
use crate::State;
use qos_relay::delivery_v3::commit::{Digest, ReplayCommit, StoreGeneration};

#[derive(Debug)]
pub(crate) enum ReplayDigestError {
    Room(uuid::Error),
    Encoding(postcard::Error),
}
impl std::fmt::Display for ReplayDigestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Room(error) => error.fmt(f),
            Self::Encoding(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for ReplayDigestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Room(error) => Some(error),
            Self::Encoding(error) => Some(error),
        }
    }
}
pub(crate) fn exact(
    state: &State,
    store: StoreGeneration,
    replay: &ReplayCommit,
) -> Result<Digest, ReplayDigestError> {
    // Preserve the existing room parse/derivation before postcard encoding.
    let workspace = qos_relay::room_workspace_id(&state.room).map_err(ReplayDigestError::Room)?;
    postcard::serialize_with_flavor(
        &(
            store,
            &state.room,
            workspace,
            replay.owner_generation,
            &replay.previous,
            &replay.publications,
            &replay.admitted_operations,
        ),
        ReplayHash(blake3::Hasher::new_derive_key(
            "qOS Cloud delivery commit v1",
        )),
    )
    .map_err(ReplayDigestError::Encoding)
}
struct ReplayHash(blake3::Hasher);
impl postcard::ser_flavors::Flavor for ReplayHash {
    type Output = Digest;
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.0.update(&[byte]);
        Ok(())
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.0.update(bytes);
        Ok(())
    }
    fn finalize(self) -> postcard::Result<Self::Output> {
        Ok(*self.0.finalize().as_bytes())
    }
}
#[cfg(test)]
mod tests;
