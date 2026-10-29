//! A Store moderation disclosure as the grant sheet states it, every field
//! of it, for the controller to page.

use crate::shell::i18n::t;

/// Keep every disclosure field and make control characters visible. The
/// controller paginates this complete text into a bounded trusted sheet;
/// neither long values nor short displays may hide content behind Allow.
pub(super) fn moderation_detail(
    preview: &tilo_action_broker::app_actions::ModerationConsentPreview,
) -> String {
    use tilo_action_broker::app_actions::ModerationConsentPurpose;
    let purpose = match preview.purpose {
        ModerationConsentPurpose::Report => "portal-moderation-report",
        ModerationConsentPurpose::Appeal => "portal-moderation-appeal",
    };
    let mut detail = t(purpose);
    for (key, value) in [
        (
            "portal-moderation-destination",
            Some(preview.destination.as_str()),
        ),
        ("portal-moderation-owner", Some(preview.owner_root.as_str())),
        (
            "portal-moderation-identity",
            Some(preview.signing_root.as_str()),
        ),
        ("portal-moderation-target", Some(preview.target.as_str())),
        ("portal-moderation-category", preview.category.as_deref()),
        ("portal-moderation-content", Some(preview.content.as_str())),
        (
            "portal-moderation-evidence",
            preview.evidence_digest.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            detail.push('\n');
            detail.push_str(&t(key));
            detail.push('\n');
            // Escape controls and directional overrides: payload text cannot
            // impersonate a trusted field label by rearranging the sheet.
            let visible: String = value.chars().flat_map(|c| {
                if c.is_control() || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                    c.escape_default().collect::<Vec<_>>()
                } else {
                    vec![c]
                }
            }).collect();
            for (index, c) in visible.chars().enumerate() {
                if index > 0 && index % 56 == 0 {
                    detail.push('\n');
                }
                detail.push(c);
            }
        }
    }
    detail
}
