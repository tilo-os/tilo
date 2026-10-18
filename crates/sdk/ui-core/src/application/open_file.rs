//! A file a host hands a component to open, as an explicit launch input.
//!
//! Apps have no ambient filesystem: a host that was asked to open a file (an
//! OS file association, a page embedding the browser runtime) reads the bytes
//! itself and lends them to the component through
//! [`HostContext::provide_open_file`](super::HostContext::provide_open_file).
//! The component declares what it opens in
//! [`ApplicationDefinition::opens`](super::ApplicationDefinition::opens); a
//! file of any other type never reaches its factory.
use std::cell::RefCell;
use std::rc::Rc;

/// The largest file a host may hand a component (128 MiB).
pub const MAX_OPEN_FILE_BYTES: usize = 128 * 1024 * 1024;

/// One file to open: its name, media type and bytes.
pub struct OpenFile {
    name: String,
    media_type: String,
    bytes: Vec<u8>,
    opened: Rc<RefCell<Option<String>>>,
}

/// What the host keeps of an [`OpenFile`] it handed over: which resource the
/// component opened it as, once the component says so.
#[derive(Clone)]
pub struct OpenFileReceipt(Rc<RefCell<Option<String>>>);

impl OpenFile {
    /// A file named `name` of type `media_type` (`type/subtype`, parameters
    /// ignored), and the receipt that later reports the opened resource.
    ///
    /// # Errors
    /// Refuses an empty or control-character name, a malformed media type, or
    /// bytes beyond [`MAX_OPEN_FILE_BYTES`].
    pub fn new(
        name: &str,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> Result<(Self, OpenFileReceipt), String> {
        if name.trim().is_empty() || name.chars().any(char::is_control) {
            return Err("the file to open has no usable name".into());
        }
        let essence = media_type_essence(media_type)
            .ok_or_else(|| format!("{media_type:?} is not a media type"))?;
        if bytes.len() > MAX_OPEN_FILE_BYTES {
            return Err(format!(
                "{name} is larger than the {} a component may be handed",
                crate::format::memory(u64::try_from(MAX_OPEN_FILE_BYTES).unwrap_or(u64::MAX))
            ));
        }
        let opened = Rc::new(RefCell::new(None));
        Ok((
            Self {
                name: name.into(),
                media_type: essence,
                bytes,
                opened: opened.clone(),
            },
            OpenFileReceipt(opened),
        ))
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The lowercase `type/subtype`, without parameters.
    pub fn media_type(&self) -> &str {
        &self.media_type
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// Tell the host which of the component's resources now holds the file
    /// (the id its action surface lists), so the host can address it.
    pub fn report_opened(&self, resource: impl Into<String>) {
        *self.opened.borrow_mut() = Some(resource.into());
    }
}

impl OpenFileReceipt {
    /// The resource the component reported for the file, if it did.
    pub fn opened_resource(&self) -> Option<String> {
        self.0.borrow().clone()
    }
}

/// The lowercase `type/subtype` of a media type, `None` when malformed.
pub(crate) fn media_type_essence(media_type: &str) -> Option<String> {
    let essence = media_type.split(';').next()?.trim().to_ascii_lowercase();
    let (kind, subtype) = essence.split_once('/')?;
    let token = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "!#$&-^_.+".contains(c))
    };
    (token(kind) && token(subtype)).then_some(essence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_carries_its_normalised_type_and_reports_what_opened_it() {
        let (file, receipt) =
            OpenFile::new("The Odyssey.epub", "Application/EPUB+zip; q=1", vec![1, 2]).unwrap();
        assert_eq!(file.media_type(), "application/epub+zip");
        assert_eq!(file.name(), "The Odyssey.epub");
        assert_eq!(file.bytes(), [1, 2]);
        assert_eq!(receipt.opened_resource(), None);
        file.report_opened("ebook.book:book:0");
        assert_eq!(
            receipt.opened_resource().as_deref(),
            Some("ebook.book:book:0")
        );
    }

    #[test]
    fn malformed_names_types_and_oversized_files_are_refused() {
        assert!(OpenFile::new(" ", "text/plain", vec![]).is_err());
        assert!(OpenFile::new("a\nb", "text/plain", vec![]).is_err());
        for media_type in ["", "epub", "/zip", "text/", "text/pl ain"] {
            assert!(
                OpenFile::new("a", media_type, vec![]).is_err(),
                "{media_type:?}"
            );
        }
        assert!(OpenFile::new("a", "text/plain", vec![0; MAX_OPEN_FILE_BYTES + 1]).is_err());
    }
}
