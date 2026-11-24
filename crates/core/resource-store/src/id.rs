//! Which document a resource id lives in: the store's rule, wherever a reader or a writer has an
//! id in hand and needs the document it names.
//!
//! A resource is named by its document, then `/item` for an item of it. A document is one step,
//! except a device's, which is kept under the device (`devices/<device>/<app>/…`): what follows
//! the app is the device's own root (`root-<type>`) or an item of it, so a resource's document is
//! the whole id for a root and every step before the item's otherwise.

/// The document of a resource written as its id: its first step, or every step up to its item for
/// a document kept under a device (`devices/<device>/<app>/root-…`).
#[must_use]
pub fn doc_of(resource: &str) -> &str {
    if resource.starts_with("devices/") {
        return split_device_id(resource).map_or(resource, |(doc, _)| doc);
    }
    resource.split_once('/').map_or(resource, |(doc, _)| doc)
}

/// The item of a resource written as its id: what follows its document ([`doc_of`]), or the whole
/// of it for a document.
#[must_use]
pub fn item_of(resource: &str) -> &str {
    resource
        .strip_prefix(doc_of(resource))
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(resource)
}

/// The document and the item (if any) of a resource a device keeps (`devices/<device>/<app>/…`):
/// the document is every step before the last, which names the item, and a root the device keeps
/// (`…/root-<type>`) is the document itself. `None` for an id that names no document or no item.
#[must_use]
pub fn split_device_id(text: &str) -> Option<(&str, Option<&str>)> {
    let (head, tail) = text.rsplit_once('/')?;
    if head.is_empty() || tail.is_empty() {
        return None;
    }
    if tail.starts_with("root-") {
        return Some((text, None));
    }
    // A path's step is `field:item`; the field is never part of the item's id.
    let item = tail.split_once(':').map_or(tail, |(_, item)| item);
    (!item.is_empty()).then_some((head, Some(item)))
}

#[cfg(test)]
mod tests {
    use super::{doc_of, item_of, split_device_id};

    #[test]
    fn a_plain_id_is_one_document() {
        assert_eq!(doc_of("notes"), "notes");
        assert_eq!(item_of("notes"), "notes");
        assert_eq!(doc_of("notes/n1"), "notes");
        assert_eq!(item_of("notes/n1"), "n1");
    }

    #[test]
    fn an_item_of_a_nested_path_is_what_follows_its_document() {
        assert_eq!(doc_of("notes/n1/body"), "notes");
        assert_eq!(item_of("notes/n1/body"), "n1/body");
        assert_eq!(item_of("notes/body:text"), "body:text");
    }

    #[test]
    fn a_devices_id_keeps_its_root_under_the_device() {
        let root = "devices/d0/settings/root-display";
        assert_eq!(doc_of(root), root);
        assert_eq!(item_of(root), root);

        let item = "devices/d0/mail/root-inbox/messages:m1";
        assert_eq!(doc_of(item), "devices/d0/mail/root-inbox");
        assert_eq!(item_of(item), "messages:m1");
        assert_eq!(
            split_device_id(item),
            Some(("devices/d0/mail/root-inbox", Some("m1")))
        );
        assert_eq!(split_device_id(root), Some((root, None)));
        assert_eq!(split_device_id("devices/d0"), Some(("devices", Some("d0"))));
    }
}
