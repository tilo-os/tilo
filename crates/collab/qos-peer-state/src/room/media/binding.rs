//! Exact Chat binding data shared by legacy and retained native documents.
use super::{CallPhaseV1, ChatCallBindingV1, MediaPolicyError, RoomMediaV1, read::Error};
use crate::resources::native::NativeDocument;
use crate::room::read::{
    self, Copied, CopyValue, LegacyCopy, LegacyRead, ReadError, RoomRead, RoomValue, Selection,
    TurnCopy,
};
use automerge::resource_limits::{NativeReadSession, NativeWorkTurn};
use automerge::{ObjId, ReadDoc};

/// Every retained string carries its own destination guard. No Clone/extraction.
pub(crate) struct Binding<T> {
    app: T,
    record: T,
    channel: T,
    object: T,
    workspace: T,
}
impl<T: AsRef<str>> Binding<T> {
    pub(crate) fn matches(&self, value: &ChatCallBindingV1) -> bool {
        self.app.as_ref() == value.app_id
            && self.record.as_ref() == value.record_id
            && self.channel.as_ref() == value.channel_id
            && self.object.as_ref() == value.channel_object
            && self.workspace.as_ref() == value.chat_workspace_id
    }
}
impl Binding<String> {
    pub(super) fn into_legacy(self) -> ChatCallBindingV1 {
        ChatCallBindingV1 {
            app_id: self.app,
            record_id: self.record,
            channel_id: self.channel,
            channel_object: self.object,
            chat_workspace_id: self.workspace,
        }
    }
}
fn policy(error: MediaPolicyError) -> Error {
    Error::Policy(error)
}
fn strict_error(error: ReadError<MediaPolicyError>) -> Error {
    match error {
        ReadError::Malformed => policy(MediaPolicyError::Malformed),
        ReadError::Resource(error) => Error::Resource(error),
        ReadError::Visitor(error) => policy(error),
    }
}
fn visit_error(error: ReadError<Error>) -> Error {
    match error {
        ReadError::Malformed => policy(MediaPolicyError::Malformed),
        ReadError::Resource(error) => Error::Resource(error),
        ReadError::Visitor(error) => error,
    }
}
trait ObjectIdWrite: RoomRead {
    // None means the exact ID cannot fit the binding's existing byte limit.
    // Defer that semantic rejection until after workspace lookup, as legacy.
    fn object_id(&self, object: &Self::Object, sink: &mut [u8]) -> Result<Option<usize>, Error>;
}
impl<D: ReadDoc> ObjectIdWrite for LegacyRead<'_, D> {
    fn object_id(&self, object: &ObjId, sink: &mut [u8]) -> Result<Option<usize>, Error> {
        let ObjId::Id(counter, actor, _) = object else {
            if sink.len() < 5 {
                return Ok(None);
            }
            sink[..5].copy_from_slice(b"_root");
            return Ok(Some(5));
        };
        let digits = if *counter == 0 {
            1
        } else {
            counter.ilog10() as usize + 1
        };
        let Some(length) = actor
            .to_bytes()
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(digits + 1))
        else {
            return Ok(None);
        };
        if length > sink.len() {
            return Ok(None);
        }
        let mut value = *counter;
        for index in (0..digits).rev() {
            sink[index] = b'0' + (value % 10) as u8;
            value /= 10;
        }
        sink[digits] = b'@';
        hex::encode_to_slice(actor.to_bytes(), &mut sink[digits + 1..length])
            .map_err(|_| policy(MediaPolicyError::Malformed))?;
        Ok(Some(length))
    }
}
impl<'doc> ObjectIdWrite for NativeReadSession<'doc, '_> {
    fn object_id(&self, object: &Self::Object, sink: &mut [u8]) -> Result<Option<usize>, Error> {
        let id = object.id();
        let length = self.id_len(id).map_err(Error::Resource)?;
        if length > sink.len() {
            return Ok(None);
        }
        self.write_id(id, sink).map(Some).map_err(Error::Resource)
    }
}
fn resolve<R: ObjectIdWrite, C: CopyValue>(
    app: &str,
    record: &str,
    channel: &str,
    room: &R,
    copies: &C,
) -> Result<Binding<C::Text>, Error> {
    let channels = read::strict_map(room, &room.root(), "channels").map_err(strict_error)?;
    let object = read::strict_map(room, &channels, channel).map_err(strict_error)?;
    if read::strict_string(room, copies, &object, "kind")
        .map_err(strict_error)?
        .as_ref()
        != "voice"
    {
        return Err(policy(MediaPolicyError::Binding));
    }
    let count = room
        .visit_property(&object, "archived", Selection::All, |value| {
            if matches!(value, RoomValue::Boolean(false)) {
                Ok(())
            } else {
                Err(policy(MediaPolicyError::Binding))
            }
        })
        .map_err(visit_error)?;
    if count > 1 {
        return Err(policy(MediaPolicyError::Binding));
    }
    let app = copies.text(app).map_err(Error::Resource)?;
    let record = copies.text(record).map_err(Error::Resource)?;
    let channel = copies.text(channel).map_err(Error::Resource)?;
    let mut buffer = [0; super::MAX_OBJECT_BYTES];
    let length = room.object_id(&object, &mut buffer)?;
    let object_text = length
        .map(|length| {
            std::str::from_utf8(&buffer[..length]).map_err(|_| policy(MediaPolicyError::Malformed))
        })
        .transpose()?;
    let object_copy = object_text
        .map(|text| copies.text(text).map_err(Error::Resource))
        .transpose()?;
    let workspace =
        read::strict_string(room, copies, &object, "workspace").map_err(strict_error)?;
    let object = object_copy.ok_or_else(|| policy(MediaPolicyError::Binding))?;
    validate_fields(
        app.as_ref(),
        record.as_ref(),
        channel.as_ref(),
        object.as_ref(),
        workspace.as_ref(),
    )
    .map_err(policy)?;
    Ok(Binding {
        app,
        record,
        channel,
        object,
        workspace,
    })
}
pub(super) fn resolve_legacy(
    app: &str,
    record: &str,
    channel: &str,
    doc: &impl ReadDoc,
) -> Result<Binding<String>, MediaPolicyError> {
    if !doc.get_missing_deps(&[]).is_empty() {
        return Err(MediaPolicyError::Binding);
    }
    resolve(app, record, channel, &LegacyRead(doc), &LegacyCopy).map_err(super::read::legacy_error)
}
pub(crate) fn resolve_native(
    app: &str,
    record: &str,
    channel: &str,
    doc: &NativeDocument,
    turn: &NativeWorkTurn,
) -> Result<Binding<Copied<String>>, Error> {
    if doc.has_missing_dependencies() {
        return Err(policy(MediaPolicyError::Binding));
    }
    resolve(
        app,
        record,
        channel,
        &doc.read(turn).map_err(Error::Resource)?,
        &TurnCopy(turn),
    )
}
fn validate_selected<T: AsRef<str>>(
    policy_value: &RoomMediaV1,
    app: &str,
    record: &str,
    mut resolve: impl FnMut(&str) -> Result<Binding<T>, Error>,
) -> Result<(), Error> {
    for call in policy_value.calls.values().filter(|call| {
        call.phase == CallPhaseV1::Active
            && call.binding.app_id == app
            && call.binding.record_id == record
    }) {
        if !resolve(&call.binding.channel_id)?.matches(&call.binding) {
            return Err(policy(MediaPolicyError::Binding));
        }
    }
    Ok(())
}
pub(super) fn validate_legacy(
    policy_value: &RoomMediaV1,
    app: &str,
    record: &str,
    doc: &impl ReadDoc,
) -> Result<(), MediaPolicyError> {
    validate_selected(policy_value, app, record, |channel| {
        resolve_legacy(app, record, channel, doc).map_err(policy)
    })
    .map_err(super::read::legacy_error)
}
pub(crate) fn validate_native(
    policy: &RoomMediaV1,
    app: &str,
    record: &str,
    doc: &NativeDocument,
    turn: &NativeWorkTurn,
) -> Result<(), Error> {
    validate_selected(policy, app, record, |channel| {
        resolve_native(app, record, channel, doc, turn)
    })
}
pub(super) fn validate_fields(
    app: &str,
    record: &str,
    channel: &str,
    object: &str,
    workspace: &str,
) -> Result<(), MediaPolicyError> {
    if app != "dev.qos.chat"
        || !super::text_valid(record, super::MAX_RECORD_BYTES, true)
        || !super::text_valid(channel, super::MAX_CHANNEL_BYTES, false)
        || !super::text_valid(workspace, super::MAX_CHANNEL_BYTES, false)
        || !super::text_valid(object, super::MAX_OBJECT_BYTES, false)
    {
        return Err(MediaPolicyError::Binding);
    }
    let (counter, actor) = object.split_once('@').ok_or(MediaPolicyError::Binding)?;
    if !counter
        .as_bytes()
        .first()
        .is_some_and(|byte| (b'1'..=b'9').contains(byte))
        || !counter.bytes().all(|byte| byte.is_ascii_digit())
        || counter.parse::<u64>().is_err()
        || actor.is_empty()
        || !actor.len().is_multiple_of(2)
        || !super::lower_hex(actor)
    {
        return Err(MediaPolicyError::Binding);
    }
    Ok(())
}
#[cfg(test)]
mod tests;
