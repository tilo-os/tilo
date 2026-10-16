//! Direct text admission from the supervisor's decrypted SDK event stream.
use super::{Arc, Client, Error, LiveBinding, Route, WhatsappRuntime, Zeroizing, wa};
use whatsapp_rust::wacore::types::{
    events::InboundMessage,
    message::{EditAttribute, MessageCategory},
};
use zeroize::Zeroize;

/// Current host custody and reviewed direct route; never supplied by an app.
pub struct IncomingContext<'a> {
    pub accounts: &'a accounts::AccountStore,
    pub binding: &'a LiveBinding,
    pub source: &'a Arc<Client>,
    pub route: &'a Route,
}

/// Only the bounded text projection leaves the SDK's encryption boundary.
/// No Debug implementation: diagnostics must not include private contents.
pub struct IncomingText {
    pub id: String,
    pub text: Zeroizing<String>,
    pub timestamp_ms: u64,
}

impl WhatsappRuntime {
    /// Validate a decrypted direct text event from this exact SDK instance.
    /// Alternate phone/LID identities, edits and disappearing/view-once/media
    /// wrappers require their own semantics and are never flattened to text.
    /// The supervisor must durably store accepted events before acknowledging
    /// them; this parser does not register a subscription or send an ACK.
    /// # Errors
    /// Refuses foreign sources, stale custody, non-direct or unsupported data.
    pub fn incoming_text(
        &self,
        context: &IncomingContext<'_>,
        event: &InboundMessage,
    ) -> Result<IncomingText, Error> {
        if !self.owns_source(context.source) {
            return Err(Error::WrongAccount);
        }
        crate::routing::check(
            context.accounts,
            context.route,
            &self.owner,
            context.binding,
        )
        .map_err(|_| Error::WrongAccount)?;
        crate::whatsapp::check_route(context.route, &self.owner.account)?;
        let info = &event.info;
        if info.source.is_from_me
            || info.source.is_group
            || info.source.chat.to_string() != context.route.recipient()
            || info.source.sender.to_string() != context.route.recipient()
            || info.source.broadcast_list_owner.is_some()
        {
            return Err(Error::InvalidIdentity);
        }
        if info.ephemeral_expiration.is_some_and(|seconds| seconds > 0)
            || info.edit != EditAttribute::Empty
            || info.category != MessageCategory::Empty
            || info.multicast
            || info.comment_target.is_some()
            || info.device_sent_meta.is_some()
            || info.unavailable_request_id.is_some()
        {
            return Err(Error::Unsupported);
        }
        let text = text_body(&event.message)?;
        let timestamp_ms = u64::try_from(info.timestamp.timestamp_millis())
            .ok()
            .filter(|time| *time > 0)
            .ok_or(Error::InvalidRequest)?;
        if !crate::identifier(&info.id) {
            return Err(Error::InvalidRequest);
        }
        // Stanza IDs are unique only within (chat, sender), not globally.
        let id = serde_json::to_string(&(
            info.source.chat.to_string(),
            info.source.sender.to_string(),
            &info.id,
        ))
        .map_err(|_| Error::InvalidRequest)?;
        if id.len() > 509 {
            return Err(Error::InvalidRequest);
        }
        Ok(IncomingText {
            id,
            text,
            timestamp_ms,
        })
    }
}

pub(crate) fn text_body(message: &wa::Message) -> Result<Zeroizing<String>, Error> {
    if !plain_fields(message) {
        return Err(Error::Unsupported);
    }
    let text = message
        .conversation
        .as_deref()
        .or_else(|| message.extended_text_message.as_option()?.text.as_deref())
        .ok_or(Error::Unsupported)?;
    if text.trim().is_empty() || text.len() > crate::signal::MAX_TEXT_BYTES {
        return Err(Error::InvalidRequest);
    }
    // Comparing with a minimal message catches other decoded payloads
    // and privacy wrappers without cloning those payloads.
    let context = message
        .message_context_info
        .as_option()
        .map(transport_context)
        .transpose()?;
    let mut plain = wa::Message::default();
    if message.conversation.is_some() {
        plain.conversation = Some(text.into());
    } else {
        plain.extended_text_message.get_or_insert_default().text = Some(text.into());
    }
    if let Some(context) = context {
        plain.message_context_info = context.into();
    }
    let supported = *message == plain;
    if let Some(value) = &mut plain.conversation {
        value.zeroize();
    }
    if let Some(value) = plain
        .extended_text_message
        .as_option_mut()
        .and_then(|field| field.text.as_mut())
    {
        value.zeroize();
    }
    if let Some(context) = plain.message_context_info.as_option_mut() {
        context.message_secret.zeroize();
        context.padding_bytes.zeroize();
    }
    if !supported {
        return Err(Error::Unsupported);
    }
    Ok(Zeroizing::new(text.into()))
}

// Ordinary direct texts carry reporting secrets/device metadata. They remain
// SDK-owned: copy only bounded fields for the comparison, zeroize the temporary
// secrets, and return none of this material in the private text projection.
fn transport_context(context: &wa::MessageContextInfo) -> Result<wa::MessageContextInfo, Error> {
    if context.limit_sharing.is_set()
        || context.limit_sharing_v2.is_set()
        || context.message_association.is_set()
        || context.bot_metadata.is_set()
        || context.weblink_render_config.is_some()
        || !context.thread_id.is_empty()
    {
        return Err(Error::Unsupported);
    }
    if context
        .message_secret
        .as_ref()
        .is_some_and(|v| v.len() != 32)
        || context
            .padding_bytes
            .as_ref()
            .is_some_and(|v| v.len() > 512)
    {
        return Err(Error::InvalidRequest);
    }
    let mut allowed = wa::MessageContextInfo {
        message_secret: context.message_secret.clone(),
        padding_bytes: context.padding_bytes.clone(),
        device_list_metadata_version: context.device_list_metadata_version,
        reporting_token_version: context.reporting_token_version,
        ..Default::default()
    };
    if let Some(device) = context.device_list_metadata.as_option() {
        if device
            .sender_key_hash
            .as_ref()
            .is_some_and(|v| v.len() > 32)
            || device
                .recipient_key_hash
                .as_ref()
                .is_some_and(|v| v.len() > 32)
            || device.sender_key_indexes.len() > 64
            || device.recipient_key_indexes.len() > 64
        {
            allowed.message_secret.zeroize();
            allowed.padding_bytes.zeroize();
            return Err(Error::InvalidRequest);
        }
        allowed.device_list_metadata = wa::DeviceListMetadata {
            sender_key_hash: device.sender_key_hash.clone(),
            sender_timestamp: device.sender_timestamp,
            sender_key_indexes: device.sender_key_indexes.clone(),
            sender_account_type: device.sender_account_type,
            receiver_account_type: device.receiver_account_type,
            recipient_key_hash: device.recipient_key_hash.clone(),
            recipient_timestamp: device.recipient_timestamp,
            recipient_key_indexes: device.recipient_key_indexes.clone(),
        }
        .into();
    }
    // Retention/sharing limits and threaded addons are not
    // transport metadata and must retain their separate semantics.
    if *context != allowed {
        allowed.message_secret.zeroize();
        allowed.padding_bytes.zeroize();
        return Err(Error::Unsupported);
    }
    Ok(allowed)
}

// MessageField equality treats an absent field and a present empty field as
// equal. Check presence explicitly, including empty privacy/media wrappers.
// Exhaustive destructuring makes a decoded SDK schema addition require review.
macro_rules! plain_text_schema {
    ($($field:ident),* $(,)?) => {
        fn plain_fields(message: &wa::Message) -> bool {
            let wa::Message { conversation, extended_text_message,
                message_context_info: _, $($field),* } = message;
            !(conversation.is_some() && extended_text_message.is_set())
                $(&& !$field.is_set())*
        }
    };
}
plain_text_schema!(
    sender_key_distribution_message,
    image_message,
    contact_message,
    location_message,
    document_message,
    audio_message,
    video_message,
    call,
    chat,
    protocol_message,
    contacts_array_message,
    highly_structured_message,
    fast_ratchet_key_sender_key_distribution_message,
    send_payment_message,
    live_location_message,
    request_payment_message,
    decline_payment_request_message,
    cancel_payment_request_message,
    template_message,
    sticker_message,
    group_invite_message,
    template_button_reply_message,
    product_message,
    device_sent_message,
    list_message,
    view_once_message,
    order_message,
    list_response_message,
    ephemeral_message,
    invoice_message,
    buttons_message,
    buttons_response_message,
    payment_invite_message,
    interactive_message,
    reaction_message,
    sticker_sync_rmr_message,
    interactive_response_message,
    poll_creation_message,
    poll_update_message,
    keep_in_chat_message,
    document_with_caption_message,
    request_phone_number_message,
    view_once_message_v2,
    enc_reaction_message,
    edited_message,
    view_once_message_v2_extension,
    poll_creation_message_v2,
    scheduled_call_creation_message,
    group_mentioned_message,
    pin_in_chat_message,
    poll_creation_message_v3,
    scheduled_call_edit_message,
    ptv_message,
    bot_invoke_message,
    call_log_messsage,
    message_history_bundle,
    enc_comment_message,
    bcall_message,
    lottie_sticker_message,
    event_message,
    enc_event_response_message,
    comment_message,
    newsletter_admin_invite_message,
    placeholder_message,
    secret_encrypted_message,
    album_message,
    event_cover_image,
    sticker_pack_message,
    status_mention_message,
    poll_result_snapshot_message,
    poll_creation_option_image_message,
    associated_child_message,
    group_status_mention_message,
    poll_creation_message_v4,
    status_add_yours,
    group_status_message,
    rich_response_message,
    status_notification_message,
    limit_sharing_message,
    bot_task_message,
    question_message,
    message_history_notice,
    group_status_message_v2,
    bot_forwarded_message,
    status_question_answer_message,
    question_reply_message,
    question_response_message,
    status_quoted_message,
    status_sticker_interaction_message,
    poll_creation_message_v5,
    newsletter_follower_invite_message_v2,
    poll_result_snapshot_message_v3,
    newsletter_admin_profile_message,
    newsletter_admin_profile_message_v2,
    spoiler_message,
    poll_creation_message_v6,
    conditional_reveal_message,
    poll_add_option_message,
    event_invite_message,
    group_root_key_share,
    payment_reminder_message,
    split_payment_message,
    newsletter_admin_profile_status_message,
    root_secret_distribute_message,
);
