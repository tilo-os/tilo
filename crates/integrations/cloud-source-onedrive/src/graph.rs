//! The Microsoft Graph resources this provider reads, and their mapping to
//! provider-neutral [`Item`]s.

use cloud_source::{
    ContentHash, HashAlgorithm, Item, ItemId, ItemKind, NativeDocument, Quota, Revision,
    RevisionInfo,
};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveItem {
    #[serde(default)]
    pub id: String,
    pub name: Option<String>,
    pub size: Option<u64>,
    pub e_tag: Option<String>,
    pub c_tag: Option<String>,
    pub last_modified_date_time: Option<String>,
    pub parent_reference: Option<ItemReference>,
    pub file: Option<FileFacet>,
    pub folder: Option<serde_json::Value>,
    pub package: Option<PackageFacet>,
    pub deleted: Option<serde_json::Value>,
    pub root: Option<serde_json::Value>,
    pub remote_item: Option<serde_json::Value>,
    pub web_url: Option<String>,
    #[serde(rename = "@microsoft.graph.downloadUrl")]
    pub download_url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemReference {
    pub id: Option<String>,
    pub drive_id: Option<String>,
    pub drive_type: Option<String>,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFacet {
    pub mime_type: Option<String>,
    pub hashes: Option<Hashes>,
}

/// The `hashes` resource. Which ones are present depends on the drive type
/// and the file's age; `quickXorHash` is the one every drive reports.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hashes {
    pub quick_xor_hash: Option<String>,
    pub sha1_hash: Option<String>,
    pub sha256_hash: Option<String>,
    pub crc32_hash: Option<String>,
}

/// A `package` facet: an item that is a folder to Graph but one document to
/// people (a `OneNote` notebook).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PackageFacet {
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Page<T> {
    #[serde(default = "Vec::new")]
    pub value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    pub next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    pub delta_link: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Drive {
    #[serde(default)]
    pub id: String,
    pub drive_type: Option<String>,
    pub quota: Option<QuotaFacet>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct QuotaFacet {
    pub used: Option<u64>,
    pub total: Option<u64>,
    pub remaining: Option<u64>,
    pub deleted: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadSessionInfo {
    pub upload_url: Option<String>,
    pub expiration_date_time: Option<String>,
    #[serde(default)]
    pub next_expected_ranges: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    #[serde(default)]
    pub id: String,
    pub last_modified_date_time: Option<String>,
    pub size: Option<u64>,
    pub last_modified_by: Option<IdentitySet>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct IdentitySet {
    pub user: Option<Identity>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Permission {
    pub link: Option<SharingLink>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharingLink {
    pub web_url: Option<String>,
}

/// A Graph error body: `{"error": {"code": "...", "message": "..."}}`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ErrorBody {
    #[serde(default)]
    pub error: ErrorDetail,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ErrorDetail {
    #[serde(default)]
    pub code: String,
}

/// The `link_extension` of a `OneNote` notebook's entry: it opens in the web
/// editor and never claims a real notebook file format.
pub const NOTEBOOK_LINK_EXTENSION: &str = "onenote";

impl DriveItem {
    pub fn is_deleted(&self) -> bool {
        self.deleted.is_some()
    }

    /// A shortcut to an item in another drive ("Add to my files"). Its id
    /// only resolves in that drive, so it is not part of this mount.
    pub fn is_shortcut(&self) -> bool {
        self.remote_item.is_some()
    }

    pub fn parent_id(&self) -> Option<&str> {
        self.parent_reference
            .as_ref()
            .and_then(|parent| parent.id.as_deref())
            .filter(|id| !id.is_empty())
    }

    pub fn is_folder(&self) -> bool {
        self.folder.is_some() || self.root.is_some()
    }

    pub fn to_item(&self) -> Option<Item> {
        if self.id.is_empty() {
            return None;
        }
        let name = self.name.clone().filter(|name| !name.is_empty())?;
        let kind = if let Some(package) = &self.package {
            ItemKind::Native(NativeDocument {
                mime: match package.kind.as_deref() {
                    Some("oneNote") => "application/onenote".to_owned(),
                    Some(other) => format!("application/x-onedrive-package-{other}"),
                    None => "application/x-onedrive-package".to_owned(),
                },
                link_extension: NOTEBOOK_LINK_EXTENSION.to_owned(),
                web_url: self.web_url.clone(),
                exports: Vec::new(),
            })
        } else if self.is_folder() {
            ItemKind::Folder
        } else {
            ItemKind::File
        };
        let file = matches!(kind, ItemKind::File);
        Some(Item {
            id: ItemId::new(self.id.clone()),
            parents: self
                .parent_id()
                .map(|parent| vec![ItemId::new(parent)])
                .unwrap_or_default(),
            name,
            size: if file {
                Some(self.size.unwrap_or(0))
            } else {
                None
            },
            modified: self
                .last_modified_date_time
                .as_deref()
                .and_then(cloud_source::http::parse_rfc3339),
            revision: self.e_tag.clone().map(Revision::new),
            hashes: self
                .file
                .as_ref()
                .and_then(|facet| facet.hashes.as_ref())
                .map(Hashes::to_content_hashes)
                .unwrap_or_default(),
            mime: self.file.as_ref().and_then(|facet| facet.mime_type.clone()),
            kind,
        })
    }
}

impl Hashes {
    pub fn to_content_hashes(&self) -> Vec<ContentHash> {
        let hex = |value: &String| value.to_ascii_lowercase();
        [
            (HashAlgorithm::QuickXor, self.quick_xor_hash.clone()),
            (HashAlgorithm::Sha256, self.sha256_hash.as_ref().map(hex)),
            (HashAlgorithm::Sha1, self.sha1_hash.as_ref().map(hex)),
            (
                HashAlgorithm::Other("crc32".into()),
                self.crc32_hash.as_ref().map(hex),
            ),
        ]
        .into_iter()
        .filter_map(|(algorithm, value)| {
            value
                .filter(|value| !value.is_empty())
                .map(|value| ContentHash { algorithm, value })
        })
        .collect()
    }
}

impl QuotaFacet {
    /// Personal drives report `used`; business drives may only report
    /// `total` and `remaining`.
    pub fn to_quota(self) -> Option<Quota> {
        let used = self.used.or_else(|| {
            let (total, remaining) = (self.total?, self.remaining?);
            Some(
                total
                    .saturating_sub(remaining)
                    .saturating_sub(self.deleted.unwrap_or(0)),
            )
        })?;
        Some(Quota {
            used,
            total: self.total,
        })
    }
}

impl Version {
    pub fn to_info(&self) -> Option<RevisionInfo> {
        (!self.id.is_empty()).then(|| RevisionInfo {
            revision: Revision::new(self.id.clone()),
            modified: self
                .last_modified_date_time
                .as_deref()
                .and_then(cloud_source::http::parse_rfc3339),
            size: self.size,
            author: self
                .last_modified_by
                .as_ref()
                .and_then(|by| by.user.as_ref())
                .and_then(|user| user.display_name.clone()),
        })
    }
}

impl UploadSessionInfo {
    /// Where the next fragment starts: the start of the first missing range
    /// (`"12345-55232"` or `"26-"`).
    pub fn next_offset(&self) -> Option<u64> {
        self.next_expected_ranges
            .iter()
            .filter_map(|range| range.split('-').next()?.trim().parse::<u64>().ok())
            .min()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DriveType {
    Personal,
    Business,
    DocumentLibrary,
}

impl DriveType {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "personal" => Some(Self::Personal),
            "business" => Some(Self::Business),
            "documentLibrary" => Some(Self::DocumentLibrary),
            _ => None,
        }
    }
}
