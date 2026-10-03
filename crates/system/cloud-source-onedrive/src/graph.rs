//! The Microsoft Graph resources this provider reads, and their mapping to
//! provider-neutral [`Item`]s.
//!
//! Only the properties used are modelled; everything else in a response is
//! ignored, so new Graph properties never break parsing.

use cloud_source::{
    ContentHash, HashAlgorithm, Item, ItemId, ItemKind, NativeDocument, Quota, Revision,
    RevisionInfo,
};
use serde::Deserialize;

/// A `driveItem`.
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

/// An `itemReference`: where an item lives.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemReference {
    pub id: Option<String>,
    pub drive_id: Option<String>,
    pub drive_type: Option<String>,
    /// Absent in delta responses.
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

/// A page of a collection (`children`, `delta`, `versions`).
#[derive(Debug, Deserialize)]
pub struct Page<T> {
    #[serde(default = "Vec::new")]
    pub value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    pub next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    pub delta_link: Option<String>,
}

/// A `drive`.
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

/// An `uploadSession`, as created and as returned by each fragment.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadSessionInfo {
    pub upload_url: Option<String>,
    pub expiration_date_time: Option<String>,
    #[serde(default)]
    pub next_expected_ranges: Vec<String>,
}

/// A `driveItemVersion`.
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

/// The `permission` `createLink` returns.
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

    /// The provider-neutral item, or `None` without an id or a name.
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
                .and_then(crate::time::parse),
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
                .and_then(crate::time::parse),
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

/// Graph drive types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DriveType {
    /// Consumer `OneDrive`.
    Personal,
    /// `OneDrive` for Business.
    Business,
    /// A `SharePoint` document library.
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

#[cfg(test)]
#[path = "samples.rs"]
mod samples;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::samples;

    fn parse<'a, T: Deserialize<'a>>(json: &'a str) -> T {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn the_drive_root_is_a_folder_without_parents() {
        let root: DriveItem = parse(samples::GET_ROOT);
        let item = root.to_item().unwrap();
        assert_eq!(item.id.as_str(), "01NKDM7HMOJTVYMDOSXFDK2QJDXCDI3WUK");
        assert!(item.is_folder());
        assert!(item.parents.is_empty());
        assert_eq!(
            item.size, None,
            "a folder's size is its contents, not a body"
        );
        assert_eq!(
            item.revision.unwrap().as_str(),
            "\"{86EB4C8E-D20D-46B9-AD41-23B8868DDA8A},1\""
        );
        assert_eq!(item.modified, crate::time::parse("2016-03-21T20:01:37Z"),);
    }

    #[test]
    fn created_folders_and_moves_name_their_parent() {
        let folder: DriveItem = parse(samples::CREATE_FOLDER);
        let item = folder.to_item().unwrap();
        assert_eq!(item.name, "New Folder");
        assert!(item.is_folder());
        assert_eq!(
            item.parents,
            [ItemId::new("E67A8F34-B0AA-46E1-8FF7-0750A29553DF")]
        );

        let moved: DriveItem = parse(samples::MOVE);
        let item = moved.to_item().unwrap();
        assert_eq!(item.name, "new-item-name.txt");
        assert_eq!(item.parents, [ItemId::new("1231203102!1011")]);
    }

    #[test]
    fn children_pages_carry_a_next_link() {
        let page: Page<DriveItem> = parse(samples::LIST_CHILDREN);
        assert_eq!(page.value.len(), 4);
        assert_eq!(page.next_link.as_deref(), Some("https://..."));
        assert!(page.value[0].file.is_some());
        assert!(page.value[1].is_folder());
        // The sample omits ids; such entries are unusable, not fatal.
        assert!(page.value[0].to_item().is_none());
    }

    #[test]
    fn delta_pages_mark_deletions_and_end_with_a_delta_link() {
        let first: Page<DriveItem> = parse(samples::DELTA_FIRST);
        assert!(first.next_link.is_some() && first.delta_link.is_none());
        assert!(first.value[2].is_deleted());
        assert!(!first.value[1].is_deleted());
        let last: Page<DriveItem> = parse(samples::DELTA_LAST);
        assert!(last.next_link.is_none());
        assert!(last.delta_link.unwrap().contains("token="));
        assert!(last.value[0].is_deleted() && last.value[0].is_folder());
        let file = last.value[1].to_item().unwrap();
        assert!(file.is_file());
        assert_eq!(file.size, Some(0));
    }

    #[test]
    fn versions_list_newest_first_with_authors() {
        let page: Page<Version> = parse(samples::VERSIONS);
        let infos: Vec<RevisionInfo> = page.value.iter().filter_map(Version::to_info).collect();
        assert_eq!(infos.len(), 3);
        assert_eq!(infos[0].revision.as_str(), "3.0");
        assert_eq!(infos[0].size, Some(123));
        assert_eq!(infos[0].author.as_deref(), Some("Ryan Gregg"));
        assert!(infos[0].modified > infos[1].modified);
    }

    #[test]
    fn upload_sessions_report_the_next_missing_range() {
        let created: UploadSessionInfo = parse(samples::UPLOAD_SESSION);
        assert_eq!(
            created.upload_url.as_deref(),
            Some("https://sn3302.up.1drv.com/up/fe6987415ace7X4e1eF866337")
        );
        assert_eq!(created.next_offset(), None);
        let fragment: UploadSessionInfo = parse(samples::UPLOAD_FRAGMENT);
        assert_eq!(fragment.next_offset(), Some(12_345));
        let open: UploadSessionInfo = parse(r#"{"nextExpectedRanges": ["26-"]}"#);
        assert_eq!(open.next_offset(), Some(26));
        let done: DriveItem = parse(samples::UPLOAD_COMPLETE);
        assert_eq!(done.to_item().unwrap().size, Some(128));
    }

    #[test]
    fn share_links_and_quotas_parse() {
        let permission: Permission = parse(samples::CREATE_LINK);
        assert_eq!(
            permission.link.unwrap().web_url.as_deref(),
            Some("https://1drv.ms/A6913278E564460AA616C71B28AD6EB6")
        );
        let drive: Drive = parse(samples::GET_DRIVE);
        assert_eq!(
            DriveType::parse(drive.drive_type.as_deref().unwrap()),
            Some(DriveType::Business)
        );
        let quota = drive.quota.unwrap().to_quota().unwrap();
        assert_eq!(quota.total, Some(1_099_511_627_776));
        assert_eq!(quota.used, 1_099_511_627_776 - 1_099_447_353_539 - 256_938);
    }

    #[test]
    fn errors_expose_their_code() {
        let body: ErrorBody = parse(samples::ERROR);
        assert_eq!(body.error.code, "invalidRange");
    }

    #[test]
    fn hashes_and_notebooks_map_to_neutral_types() {
        let item: DriveItem = parse(
            r#"{"id": "A!1", "name": "a.bin", "size": 3, "eTag": "\"{A},2\"",
                "parentReference": {"id": "A!0"},
                "file": {"mimeType": "application/octet-stream", "hashes": {
                    "quickXorHash": "SgAAAAAAAAAAAAAAAQAAAAAAAAA=",
                    "sha256Hash": "ABCDEF", "sha1Hash": "0123"}}}"#,
        );
        let item = item.to_item().unwrap();
        assert_eq!(
            item.hash(&HashAlgorithm::QuickXor),
            Some("SgAAAAAAAAAAAAAAAQAAAAAAAAA=")
        );
        assert_eq!(item.hash(&HashAlgorithm::Sha256), Some("abcdef"));
        assert_eq!(item.mime.as_deref(), Some("application/octet-stream"));

        let notebook: DriveItem = parse(
            r#"{"id": "A!2", "name": "Notes", "package": {"type": "oneNote"},
                "webUrl": "https://onedrive.live.com/notes"}"#,
        );
        let ItemKind::Native(document) = notebook.to_item().unwrap().kind else {
            panic!("a notebook is a native document");
        };
        assert_eq!(document.link_extension, NOTEBOOK_LINK_EXTENSION);
        assert_eq!(
            document.web_url.as_deref(),
            Some("https://onedrive.live.com/notes")
        );
    }
}
