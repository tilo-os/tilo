//! Exact identity encoding; no native owner or actor allocation escapes.
use super::*;
use crate::exid::BorrowedExId;
impl<'doc> NativeReadSession<'doc, '_> {
    fn binary_identity(
        &self,
        id: NativeId<'doc>,
    ) -> Result<BorrowedExId<'doc>, NativeResourceError> {
        self.check_id(id)?;
        if id.id == ObjId::root().0 {
            return Ok(BorrowedExId::Root);
        }
        let actor = self
            .document
            .doc
            .ops
            .get_actor_safe(id.id.actor())
            .ok_or(NativeResourceError::Invalid)?;
        Ok(BorrowedExId::Id {
            counter: id.id.counter(),
            actor: actor.to_bytes(),
            index: id.id.actor(),
        })
    }
    /// Required bytes for the existing canonical binary object ID encoding.
    /// Unlike semantic identity, this includes the cached actor-index hint.
    pub fn id_bytes_len(&self, id: NativeId<'doc>) -> Result<usize, NativeResourceError> {
        self.binary_identity(id)?
            .encoded_len()
            .ok_or(NativeResourceError::WorkBytes)
    }
    /// Writes the existing binary encoding without allocating. A short sink
    /// is refused before any bytes are written. Caller storage needs its own
    /// current-turn output reservation when it is heap backed.
    pub fn write_id_bytes(
        &self,
        id: NativeId<'doc>,
        sink: &mut [u8],
    ) -> Result<usize, NativeResourceError> {
        let value = self.binary_identity(id)?;
        let length = value.encoded_len().ok_or(NativeResourceError::WorkBytes)?;
        if sink.len() < length {
            return Err(NativeResourceError::WorkBytes);
        }
        value
            .write(&mut sink[..length])
            .map_err(|_| NativeResourceError::Invalid)?;
        Ok(length)
    }
    /// Lends semantic counter/actor identity, excluding the cached actor-index
    /// hint. The root is None. Actor bytes cannot escape this callback loan.
    pub fn visit_id_parts<R>(
        &self,
        id: NativeId<'doc>,
        visitor: impl for<'a> FnOnce(Option<(u64, &'a [u8])>) -> R,
    ) -> Result<R, NativeResourceError> {
        Ok(match self.binary_identity(id)? {
            BorrowedExId::Root => visitor(None),
            BorrowedExId::Id { counter, actor, .. } => visitor(Some((counter, actor))),
        })
    }
}
