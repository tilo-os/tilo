//! Drive API v3 wire types and their mapping onto cloud-source items.
//!
//! Drive encodes `int64` fields (`size`, `version`, quota numbers) as JSON
//! strings; the deserialisers here accept either form so a proxy or a future
//! API revision that sends numbers does not break parsing.

use std::time::Duration;

use cloud_source::{
    ContentHash, Error, ExportFormat, HashAlgorithm, Item, ItemId, ItemKind, NativeDocument,
    Revision, http,
};
use serde::{Deserialize, Deserializer};

pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
pub const SHORTCUT_MIME: &str = "application/vnd.google-apps.shortcut";
const GOOGLE_APPS_PREFIX: &str = "application/vnd.google-apps.";

/// The file fields every item-returning request asks for. Drive returns only
/// `id`, `name`, `mimeType` and `kind` unless told otherwise.
pub const FILE_FIELDS: &str = "id,name,mimeType,parents,size,modifiedTime,version,headRevisionId,md5Checksum,sha1Checksum,sha256Checksum,trashed,webViewLink,driveId";

/// Fields a precondition check needs.
pub const REVISION_FIELDS: &str = "id,mimeType,parents,version,headRevisionId,trashed";

/// One Drive `files` resource, restricted to the fields in [`FILE_FIELDS`].
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct File {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub parents: Vec<String>,
    #[serde(deserialize_with = "int64")]
    pub size: Option<u64>,
    pub modified_time: Option<String>,
    #[serde(deserialize_with = "int64")]
    pub version: Option<u64>,
    pub head_revision_id: Option<String>,
    pub md5_checksum: Option<String>,
    pub sha1_checksum: Option<String>,
    pub sha256_checksum: Option<String>,
    pub trashed: bool,
    pub web_view_link: Option<String>,
    pub drive_id: Option<String>,
}

impl File {
    pub fn is_folder(&self) -> bool {
        self.mime_type == FOLDER_MIME
    }

    /// Docs, Sheets, Slides, Forms, shortcuts…: no downloadable body.
    pub fn is_native(&self) -> bool {
        !self.is_folder() && self.mime_type.starts_with(GOOGLE_APPS_PREFIX)
    }

    /// The precondition token: the head revision of a blob file (it moves
    /// exactly when the content does), else the file's `version` (which
    /// moves on any change, for folders and native documents).
    pub fn revision(&self) -> Option<Revision> {
        match (&self.head_revision_id, self.version) {
            (Some(head), _) if !self.is_native() && !self.is_folder() => {
                Some(Revision::new(head.clone()))
            }
            (_, Some(version)) => Some(Revision::new(format!("v{version}"))),
            (_, None) => None,
        }
    }

    pub fn kind(&self) -> ItemKind {
        if self.is_folder() {
            ItemKind::Folder
        } else if self.is_native() {
            ItemKind::Native(native_document(&self.mime_type, self.web_view_link.clone()))
        } else {
            ItemKind::File
        }
    }

    pub fn hashes(&self) -> Vec<ContentHash> {
        [
            (HashAlgorithm::Md5, &self.md5_checksum),
            (HashAlgorithm::Sha1, &self.sha1_checksum),
            (HashAlgorithm::Sha256, &self.sha256_checksum),
        ]
        .into_iter()
        .filter_map(|(algorithm, value)| {
            value.as_ref().map(|value| ContentHash {
                algorithm,
                value: value.to_ascii_lowercase(),
            })
        })
        .collect()
    }

    /// The item, with parent ids passed through `parent` (the source maps
    /// the real My Drive root id to its `root` alias there).
    pub fn to_item(&self, parent: impl Fn(&str) -> ItemId) -> Item {
        let kind = self.kind();
        Item {
            id: ItemId::new(self.id.clone()),
            parents: self.parents.iter().map(|p| parent(p)).collect(),
            name: self.name.clone(),
            size: if kind == ItemKind::File {
                Some(self.size.unwrap_or(0))
            } else {
                None
            },
            modified: self.modified_time.as_deref().and_then(http::parse_rfc3339),
            revision: self.revision(),
            hashes: if kind == ItemKind::File {
                self.hashes()
            } else {
                Vec::new()
            },
            mime: Some(self.mime_type.clone()),
            kind,
        }
    }
}

const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const PPTX: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
const ODT: &str = "application/vnd.oasis.opendocument.text";
const ODS: &str = "application/x-vnd.oasis.opendocument.spreadsheet";
const ODP: &str = "application/vnd.oasis.opendocument.presentation";
const PDF: &str = "application/pdf";

/// The link extension and export formats of a Google-native type.
pub fn native_document(mime: &str, web_url: Option<String>) -> NativeDocument {
    let kind = mime.strip_prefix(GOOGLE_APPS_PREFIX).unwrap_or(mime);
    let (link_extension, exports): (&str, &[(&str, &str)]) = match kind {
        "document" => ("gdoc", &[(DOCX, "docx"), (ODT, "odt"), (PDF, "pdf")]),
        "spreadsheet" => ("gsheet", &[(XLSX, "xlsx"), (ODS, "ods"), (PDF, "pdf")]),
        "presentation" => ("gslides", &[(PPTX, "pptx"), (ODP, "odp"), (PDF, "pdf")]),
        "drawing" => (
            "gdraw",
            &[(PDF, "pdf"), ("image/png", "png"), ("image/svg+xml", "svg")],
        ),
        "script" => (
            "gscript",
            &[("application/vnd.google-apps.script+json", "json")],
        ),
        "form" => ("gform", &[]),
        "site" => ("gsite", &[]),
        "map" => ("gmap", &[]),
        "jam" => ("gjam", &[(PDF, "pdf")]),
        _ => ("glink", &[]),
    };
    NativeDocument {
        mime: mime.to_owned(),
        link_extension: link_extension.to_owned(),
        web_url,
        exports: exports
            .iter()
            .map(|(mime, extension)| ExportFormat {
                mime: (*mime).to_owned(),
                extension: (*extension).to_owned(),
            })
            .collect(),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileList {
    pub files: Vec<File>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChangeList {
    pub changes: Vec<ChangeEntry>,
    pub next_page_token: Option<String>,
    pub new_start_page_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChangeEntry {
    /// `file` or `drive` (a shared drive's own metadata changed).
    pub change_type: Option<String>,
    pub removed: bool,
    pub file_id: Option<String>,
    pub file: Option<File>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StartPageToken {
    pub start_page_token: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RevisionList {
    pub revisions: Vec<RevisionEntry>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RevisionEntry {
    pub id: String,
    pub modified_time: Option<String>,
    #[serde(deserialize_with = "int64")]
    pub size: Option<u64>,
    pub keep_forever: bool,
    pub last_modifying_user: Option<User>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct User {
    pub display_name: Option<String>,
    pub email_address: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct About {
    pub storage_quota: StorageQuota,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StorageQuota {
    /// Absent for unlimited storage.
    #[serde(deserialize_with = "int64")]
    pub limit: Option<u64>,
    #[serde(deserialize_with = "int64")]
    pub usage: Option<u64>,
}

/// The JSON error envelope of every Google API.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ErrorDetail {
    pub code: u16,
    pub message: String,
    pub status: Option<String>,
    pub errors: Vec<ErrorItem>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ErrorItem {
    pub domain: Option<String>,
    pub reason: Option<String>,
    pub location: Option<String>,
    pub location_type: Option<String>,
}

impl ErrorBody {
    /// Parse an error body; an unparsable one (an HTML proxy page) is empty.
    pub fn parse(body: &[u8]) -> Self {
        serde_json::from_slice(body).unwrap_or_default()
    }

    pub fn reason(&self) -> Option<&str> {
        self.error.errors.iter().find_map(|e| e.reason.as_deref())
    }

    /// Whether the error names the `pageToken` parameter: the token of a
    /// change feed or listing is no longer valid.
    pub fn is_page_token_error(&self) -> bool {
        self.error
            .errors
            .iter()
            .any(|e| e.location.as_deref() == Some("pageToken"))
            || self.error.message.contains("pageToken")
            || self
                .error
                .message
                .to_ascii_lowercase()
                .contains("page token")
    }
}

/// Map a failed response to a provider-neutral error: Drive's 403 reasons
/// here, every other status by [`http::classify`]. A 401 maps to
/// [`Error::Unauthorized`]; reporting the rejection is the caller's job.
pub fn classify(status: u16, retry_after: Option<Duration>, body: &[u8]) -> Error {
    let parsed = ErrorBody::parse(body);
    let reason = parsed.reason().unwrap_or("");
    match (status, reason) {
        (401, _) => Error::Unauthorized,
        // A per-user rate limit, or the project's daily quota (no point
        // retrying within the hour). Without a `Retry-After` the delay is
        // the daemon's choice.
        (
            403,
            "userRateLimitExceeded" | "rateLimitExceeded" | "dailyLimitExceeded" | "quotaExceeded",
        ) => Error::RateLimited { retry_after },
        (403, "storageQuotaExceeded") => Error::InsufficientStorage,
        (403, "fileNotDownloadable") => Error::Unsupported("downloading a native document"),
        (403, "exportSizeLimitExceeded") => Error::TooLarge,
        (status, _) => http::classify(status, retry_after).unwrap_or(Error::Status(status)),
    }
}

/// The committed length from a resumable upload's `Range: bytes=0-<last>`
/// (a 308 without `Range` means nothing was committed).
pub fn parse_committed(range: Option<&str>) -> Option<u64> {
    let Some(range) = range else { return Some(0) };
    let last = range.trim().strip_prefix("bytes=0-")?;
    last.trim().parse::<u64>().ok()?.checked_add(1)
}

/// Quote `value` for a Drive search query string literal.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len().saturating_add(2));
    out.push('\'');
    for c in value.chars() {
        if matches!(c, '\'' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Accepts an `int64` as Drive sends it (a string) or as a number.
fn int64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Text(String),
        Number(u64),
    }
    Ok(match Option::<Wire>::deserialize(deserializer)? {
        None => None,
        Some(Wire::Number(n)) => Some(n),
        Some(Wire::Text(text)) => text.trim().parse().ok(),
    })
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    /// A `files.get` response in the shape of the Drive v3 reference
    /// (`int64` fields as strings).
    const FILE_SAMPLE: &str = r#"{
      "kind": "drive#file",
      "id": "1Z7wD3tS8yQK0qL5nJtYF0rWm2c4vX9pE",
      "name": "Quarterly report.pdf",
      "mimeType": "application/pdf",
      "parents": ["0AFa1bC2dE3fGUk9PVA"],
      "size": "283745",
      "modifiedTime": "2024-03-18T09:41:07.512Z",
      "version": "17",
      "headRevisionId": "0B6Pq2x5vSfJ9ZWtQbjVYU3NjQkxPS2VUV0FnU2p0OE9HZ1NBPQ",
      "md5Checksum": "5d41402abc4b2a76b9719d911017c592",
      "sha1Checksum": "AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D",
      "sha256Checksum": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
      "trashed": false,
      "webViewLink": "https://drive.google.com/file/d/1Z7wD3tS8yQK0qL5nJtYF0rWm2c4vX9pE/view?usp=drivesdk"
    }"#;

    #[test]
    fn file_resources_parse_with_string_int64_fields() {
        let file: File = serde_json::from_str(FILE_SAMPLE).unwrap();
        let item = file.to_item(|p: &str| ItemId::new(p));
        assert_eq!(item.size, Some(283_745));
        assert_eq!(item.kind, ItemKind::File);
        assert_eq!(
            item.revision.as_ref().map(Revision::as_str),
            Some("0B6Pq2x5vSfJ9ZWtQbjVYU3NjQkxPS2VUV0FnU2p0OE9HZ1NBPQ")
        );
        assert_eq!(
            item.hash(&HashAlgorithm::Sha1),
            Some("aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d")
        );
        assert_eq!(
            item.hash(&HashAlgorithm::Md5),
            Some("5d41402abc4b2a76b9719d911017c592")
        );
        assert_eq!(item.parents, [ItemId::new("0AFa1bC2dE3fGUk9PVA")]);
        let modified = item.modified.unwrap();
        let since = modified.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(since.as_secs(), 1_710_754_867);
        assert_eq!(since.subsec_millis(), 512);
    }

    #[test]
    fn native_documents_and_folders_are_told_apart() {
        let doc: File = serde_json::from_str(
            r#"{"id":"1dOc","name":"Plan","mimeType":"application/vnd.google-apps.document",
                "parents":["root"],"version":"4","size":"1024",
                "webViewLink":"https://docs.google.com/document/d/1dOc/edit?usp=drivesdk"}"#,
        )
        .unwrap();
        let item = doc.to_item(|p: &str| ItemId::new(p));
        let ItemKind::Native(native) = &item.kind else {
            panic!("{item:?}")
        };
        assert_eq!(native.link_extension, "gdoc");
        assert_eq!(
            native.web_url.as_deref(),
            Some("https://docs.google.com/document/d/1dOc/edit?usp=drivesdk")
        );
        let extensions: Vec<&str> = native
            .exports
            .iter()
            .map(|e| e.extension.as_str())
            .collect();
        assert_eq!(extensions, ["docx", "odt", "pdf"]);
        assert_eq!(item.size, None, "a native document has no byte body");
        assert_eq!(item.revision, Some(Revision::new("v4")));

        let folder: File = serde_json::from_str(
            r#"{"id":"0fOl","name":"Work","mimeType":"application/vnd.google-apps.folder","version":"9"}"#,
        )
        .unwrap();
        assert!(folder.to_item(|p: &str| ItemId::new(p)).is_folder());
        assert_eq!(folder.revision(), Some(Revision::new("v9")));
        for (mime, extension) in [
            ("application/vnd.google-apps.spreadsheet", "gsheet"),
            ("application/vnd.google-apps.presentation", "gslides"),
            ("application/vnd.google-apps.drawing", "gdraw"),
            ("application/vnd.google-apps.form", "gform"),
            (SHORTCUT_MIME, "glink"),
        ] {
            assert_eq!(native_document(mime, None).link_extension, extension);
        }
    }

    /// The `changes.list` example of the Drive v3 guide "Retrieve changes".
    #[test]
    fn change_pages_parse_removals_and_tokens() {
        let page: ChangeList = serde_json::from_str(
            r#"{
              "kind": "drive#changeList",
              "newStartPageToken": "8930",
              "changes": [
                {"kind": "drive#change", "removed": false, "fileId": "1aB", "changeType": "file",
                 "time": "2024-03-18T09:41:07.512Z",
                 "file": {"kind":"drive#file","id":"1aB","name":"a.txt","mimeType":"text/plain","size":"3","parents":["0AP"]}},
                {"kind": "drive#change", "removed": true, "fileId": "1cD", "changeType": "file",
                 "time": "2024-03-18T09:42:00.000Z"},
                {"kind": "drive#change", "changeType": "drive", "driveId": "0AdR", "removed": false,
                 "time": "2024-03-18T09:43:00.000Z"}
              ]
            }"#,
        )
        .unwrap();
        assert_eq!(page.new_start_page_token.as_deref(), Some("8930"));
        assert!(page.next_page_token.is_none());
        assert_eq!(page.changes.len(), 3);
        assert_eq!(page.changes[0].file.as_ref().unwrap().size, Some(3));
        assert!(page.changes[1].removed && page.changes[1].file.is_none());
        assert_eq!(page.changes[2].change_type.as_deref(), Some("drive"));
    }

    #[test]
    fn revisions_and_quota_parse() {
        let list: RevisionList = serde_json::from_str(
            r#"{"kind":"drive#revisionList","revisions":[
                {"kind":"drive#revision","id":"0B1","mimeType":"text/plain","modifiedTime":"2024-01-02T03:04:05Z",
                 "keepForever":false,"size":"5","lastModifyingUser":{"kind":"drive#user","displayName":"Ada Lovelace","emailAddress":"ada@example.com"}},
                {"kind":"drive#revision","id":"0B2","modifiedTime":"2024-01-03T03:04:05+01:00","keepForever":true,"size":"14"}]}"#,
        )
        .unwrap();
        assert_eq!(list.revisions[0].size, Some(5));
        assert_eq!(
            list.revisions[0]
                .last_modifying_user
                .as_ref()
                .and_then(|u| u.display_name.as_deref()),
            Some("Ada Lovelace")
        );
        assert!(list.revisions[1].keep_forever);
        assert_eq!(
            http::parse_rfc3339("2024-01-03T03:04:05+01:00"),
            http::parse_rfc3339("2024-01-03T02:04:05Z")
        );

        let about: About = serde_json::from_str(
            r#"{"storageQuota":{"limit":"16106127360","usage":"5368709120","usageInDrive":"4294967296","usageInDriveTrash":"1048576"}}"#,
        )
        .unwrap();
        assert_eq!(about.storage_quota.limit, Some(16_106_127_360));
        assert_eq!(about.storage_quota.usage, Some(5_368_709_120));
        let unlimited: About = serde_json::from_str(r#"{"storageQuota":{"usage":"42"}}"#).unwrap();
        assert_eq!(unlimited.storage_quota.limit, None);
    }

    #[test]
    fn documented_error_bodies_classify() {
        let user_rate = r#"{"error":{"errors":[{"domain":"usageLimits","reason":"userRateLimitExceeded","message":"User Rate Limit Exceeded"}],"code":403,"message":"User Rate Limit Exceeded"}}"#;
        assert!(matches!(
            classify(403, None, user_rate.as_bytes()),
            Error::RateLimited { retry_after: None }
        ));
        let rate = r#"{"error":{"errors":[{"domain":"usageLimits","reason":"rateLimitExceeded","message":"Rate Limit Exceeded"}],"code":429,"message":"Rate Limit Exceeded"}}"#;
        assert!(matches!(
            classify(429, http::parse_retry_after("7", SystemTime::now()), rate.as_bytes()),
            Error::RateLimited { retry_after: Some(d) } if d == Duration::from_secs(7)
        ));
        let auth = r#"{"error":{"errors":[{"domain":"global","reason":"authError","message":"Invalid Credentials","locationType":"header","location":"Authorization"}],"code":401,"message":"Invalid Credentials"}}"#;
        assert!(matches!(
            classify(401, None, auth.as_bytes()),
            Error::Unauthorized
        ));
        let not_found = r#"{"error":{"errors":[{"domain":"global","reason":"notFound","message":"File not found: 1x.","locationType":"parameter","location":"fileId"}],"code":404,"message":"File not found: 1x."}}"#;
        assert!(matches!(
            classify(404, None, not_found.as_bytes()),
            Error::NotFound
        ));
        let denied = r#"{"error":{"code":403,"message":"The user does not have sufficient permissions for this file.","errors":[{"message":"The user does not have sufficient permissions for this file.","domain":"global","reason":"insufficientFilePermissions"}]}}"#;
        assert!(matches!(
            classify(403, None, denied.as_bytes()),
            Error::Forbidden
        ));
        let storage = r#"{"error":{"errors":[{"domain":"usageLimits","reason":"storageQuotaExceeded","message":"The user's Drive storage quota has been exceeded."}],"code":403,"message":"The user's Drive storage quota has been exceeded."}}"#;
        assert!(matches!(
            classify(403, None, storage.as_bytes()),
            Error::InsufficientStorage
        ));
        let native = r#"{"error":{"code":403,"message":"Only files with binary content can be downloaded. Use Export with Docs Editors files.","errors":[{"message":"Only files with binary content can be downloaded. Use Export with Docs Editors files.","domain":"global","reason":"fileNotDownloadable","location":"alt","locationType":"parameter"}]}}"#;
        assert!(matches!(
            classify(403, None, native.as_bytes()),
            Error::Unsupported(_)
        ));
        let token = r#"{"error":{"code":400,"message":"Invalid Value","errors":[{"message":"Invalid Value","domain":"global","reason":"invalid","location":"pageToken","locationType":"parameter"}]}}"#;
        assert!(ErrorBody::parse(token.as_bytes()).is_page_token_error());
        assert!(!ErrorBody::parse(not_found.as_bytes()).is_page_token_error());
        assert!(matches!(
            classify(503, None, b"<html>backend</html>"),
            Error::RateLimited { retry_after: None }
        ));
    }

    #[test]
    fn range_headers_parse() {
        assert_eq!(parse_committed(None), Some(0));
        assert_eq!(parse_committed(Some("bytes=0-262143")), Some(262_144));
        assert_eq!(parse_committed(Some("bytes=5-9")), None);
    }

    #[test]
    fn query_literals_escape_quotes_and_backslashes() {
        assert_eq!(quote(r"it's a\b"), r"'it\'s a\\b'");
    }
}
