//! Wire shapes of the drivews and docws endpoints, as rclone's
//! `iclouddrive/api` and pyicloud read them. Every field is optional:
//! Apple adds and drops fields without notice.

use serde::Deserialize;

/// The `CloudDocs` zone of ordinary iCloud Drive items. App libraries
/// (Pages, Keynote, third-party apps) live in zones of their own.
pub const DEFAULT_ZONE: &str = "com.apple.CloudDocs";
/// The iCloud Drive root folder.
pub const ROOT_ID: &str = "FOLDER::com.apple.CloudDocs::root";
/// Where trashed items go.
pub const TRASH_ID: &str = "FOLDER::com.apple.CloudDocs::TRASH_ROOT";

/// One item as drivews describes it; folders listed with their children.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DriveItem {
    /// `<TYPE>::<zone>::<docwsid>`.
    pub drivewsid: String,
    pub docwsid: String,
    pub zone: String,
    pub name: String,
    pub extension: Option<String>,
    pub parent_id: Option<String>,
    pub etag: Option<String>,
    /// `FILE`, `FOLDER`, `APP_LIBRARY`, `APP_CONTAINER`.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub size: Option<u64>,
    pub date_created: Option<String>,
    pub date_modified: Option<String>,
    pub date_changed: Option<String>,
    pub items: Option<Vec<DriveItem>>,
    /// Per-item result of a write (`OK`, `ETAG_CONFLICT`, …); absent on
    /// reads, or a not-found marker for an unknown id.
    pub status: Option<String>,
}

impl DriveItem {
    pub fn is_folder(&self) -> bool {
        matches!(
            self.kind.as_deref(),
            Some("FOLDER" | "APP_LIBRARY" | "APP_CONTAINER")
        )
    }

    pub fn is_file(&self) -> bool {
        self.kind.as_deref() == Some("FILE")
    }

    /// `name.extension`, the name a person sees.
    pub fn full_name(&self) -> String {
        match self.extension.as_deref() {
            Some(extension) if !extension.is_empty() => format!("{}.{extension}", self.name),
            _ => self.name.clone(),
        }
    }
}

/// `{"items": [...]}` (renameItems, moveItems, moveItemsToTrash).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Items {
    pub items: Vec<DriveItem>,
}

/// `{"folders": [...]}` (createFolders).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Folders {
    pub folders: Vec<DriveItem>,
}

/// `download/by_id`: where the bytes are.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct DownloadInfo {
    pub data_token: Option<ContentToken>,
    pub package_token: Option<ContentToken>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ContentToken {
    pub url: String,
}

/// One entry of `upload/web`'s answer.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct UploadSlot {
    pub document_id: String,
    pub url: String,
}

/// The content server's receipt for uploaded bytes.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SingleFile {
    pub reference_checksum: String,
    pub size: u64,
    pub file_checksum: String,
    pub wrapping_key: String,
    pub receipt: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Uploaded {
    pub single_file: Option<SingleFile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct DocStatus {
    pub status_code: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Document {
    pub document_id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct UpdateResult {
    pub status: DocStatus,
    pub document: Option<Document>,
}

/// `update/documents`' answer.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct UpdateResponse {
    pub status: DocStatus,
    pub results: Vec<UpdateResult>,
}

/// `(type, zone, docwsid)` of a drivewsid; the default zone when it names
/// none.
pub fn split_id(id: &str) -> (&str, &str, &str) {
    let mut parts = id.splitn(3, "::");
    match (parts.next(), parts.next(), parts.next()) {
        (Some(kind), Some(zone), Some(doc)) => (kind, zone, doc),
        _ => ("", DEFAULT_ZONE, id),
    }
}
