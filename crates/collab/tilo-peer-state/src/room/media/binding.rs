//! Exact Chat binding data, read from any admitted document.
use super::{CallPhaseV1, ChatCallBindingV1, MediaPolicyError, RoomMediaV1, read::Error};
use crate::resources::native::NativeDocument;
use crate::room::read::{self, LegacyRead, ReadError, RoomRead, RoomValue, Selection};
use automerge::{ObjId, ReadDoc};

pub(crate) struct Binding {
    app: String,
    record: String,
    channel: String,
    object: String,
    workspace: String,
}
impl Binding {
    pub(crate) fn matches(&self, value: &ChatCallBindingV1) -> bool {
        self.app == value.app_id
            && self.record == value.record_id
            && self.channel == value.channel_id
            && self.object == value.channel_object
            && self.workspace == value.chat_workspace_id
    }

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
        ReadError::Capacity => Error::Capacity,
        ReadError::Visitor(error) => policy(error),
    }
}
fn visit_error(error: ReadError<Error>) -> Error {
    match error {
        ReadError::Malformed => policy(MediaPolicyError::Malformed),
        ReadError::Capacity => Error::Capacity,
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
            let Some(root) = sink.get_mut(..5) else {
                return Ok(None);
            };
            root.copy_from_slice(b"_root");
            return Ok(Some(5));
        };
        // A u64 has at most 20 decimal digits, so these steps are exact.
        let digits = counter
            .checked_ilog10()
            .map_or(1, |log| (log as usize).saturating_add(1));
        let Some(length) = actor
            .to_bytes()
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(digits.saturating_add(1)))
        else {
            return Ok(None);
        };
        let Some((number, rest)) = sink
            .get_mut(..length)
            .and_then(|target| target.split_at_mut_checked(digits))
        else {
            return Ok(None);
        };
        let Some((at, hex_digits)) = rest.split_first_mut() else {
            return Ok(None);
        };
        let mut value = *counter;
        for slot in number.iter_mut().rev() {
            *slot = b'0'.saturating_add((value % 10) as u8);
            value /= 10;
        }
        *at = b'@';
        hex::encode_to_slice(actor.to_bytes(), hex_digits)
            .map_err(|_| policy(MediaPolicyError::Malformed))?;
        Ok(Some(length))
    }
}
fn resolve<R: ObjectIdWrite>(
    app: &str,
    record: &str,
    channel: &str,
    room: &R,
) -> Result<Binding, Error> {
    // Chat keeps its channels as a collection: each entry holds its order
    // key and, under `v`, the channel itself.
    let channels = read::strict_map(room, &room.root(), "channels").map_err(strict_error)?;
    let entry = read::strict_map(room, &channels, channel).map_err(strict_error)?;
    let object = read::strict_map(room, &entry, "v").map_err(strict_error)?;
    if read::strict_string(room, &object, "kind").map_err(strict_error)? != "voice" {
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
    let app = app.to_owned();
    let record = record.to_owned();
    let channel = channel.to_owned();
    let mut buffer = [0; super::MAX_OBJECT_BYTES];
    let length = room.object_id(&object, &mut buffer)?;
    let object_text = length
        .map(|length| {
            buffer
                .get(..length)
                .and_then(|text| std::str::from_utf8(text).ok())
                .ok_or_else(|| policy(MediaPolicyError::Malformed))
        })
        .transpose()?;
    let object_copy = object_text.map(str::to_owned);
    let workspace = read::strict_string(room, &object, "workspace").map_err(strict_error)?;
    let object = object_copy.ok_or_else(|| policy(MediaPolicyError::Binding))?;
    validate_fields(&app, &record, &channel, &object, &workspace).map_err(policy)?;
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
) -> Result<Binding, MediaPolicyError> {
    if !doc.get_missing_deps(&[]).is_empty() {
        return Err(MediaPolicyError::Binding);
    }
    resolve(app, record, channel, &LegacyRead(doc)).map_err(super::read::legacy_error)
}
pub(crate) fn resolve_native(
    app: &str,
    record: &str,
    channel: &str,
    doc: &NativeDocument,
) -> Result<Binding, Error> {
    if doc.has_missing_dependencies() {
        return Err(policy(MediaPolicyError::Binding));
    }
    resolve(app, record, channel, &doc.read())
}
fn validate_selected(
    policy_value: &RoomMediaV1,
    app: &str,
    record: &str,
    mut resolve: impl FnMut(&str) -> Result<Binding, Error>,
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
) -> Result<(), Error> {
    validate_selected(policy, app, record, |channel| {
        resolve_native(app, record, channel, doc)
    })
}
pub(super) fn validate_fields(
    app: &str,
    record: &str,
    channel: &str,
    object: &str,
    workspace: &str,
) -> Result<(), MediaPolicyError> {
    // A binding names an existing Chat record.
    if app != "so.tilo.chat"
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
