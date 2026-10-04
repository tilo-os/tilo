//! The container manager's document: the new-container draft.
//!
//! The engine (containers, images, volumes, distroboxes, their stats and
//! logs) is this device's podman, mirrored into memory and never written
//! here; where the user is in the app (section, selection, detail tab) is
//! the window's own. What the document holds is what a person fills in and
//! may finish on another device or with someone else: the create form, one
//! collaborative text per field.
//!
//! ```text
//! root
//! └── "create" (Map)
//!     ├── "name"    (Text)
//!     ├── "image"   (Text)
//!     ├── "ports"   (Text)
//!     ├── "env"     (Text)
//!     ├── "volumes" (Text)
//!     └── "command" (Text)
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use ui::tilo_actions::ActionError;

/// The root key of the draft.
const CREATE_KEY: &str = "create";

/// The editable fields of the create-container form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CreateField {
    Name,
    Image,
    Ports,
    Env,
    Volumes,
    Command,
}

impl CreateField {
    pub fn key(self) -> &'static str {
        match self {
            CreateField::Name => "name",
            CreateField::Image => "image",
            CreateField::Ports => "ports",
            CreateField::Env => "env",
            CreateField::Volumes => "volumes",
            CreateField::Command => "command",
        }
    }
    /// The field's name, as a title.
    pub fn title(self) -> &'static str {
        match self {
            CreateField::Name => "Name",
            CreateField::Image => "Image",
            CreateField::Ports => "Ports",
            CreateField::Env => "Environment",
            CreateField::Volumes => "Volumes",
            CreateField::Command => "Command",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            CreateField::Name => "Name",
            CreateField::Image => "Image",
            CreateField::Ports => "Ports (host:container, comma-separated)",
            CreateField::Env => "Environment (KEY=value, comma-separated)",
            CreateField::Volumes => "Volumes (name:/path, comma-separated)",
            CreateField::Command => "Command (optional)",
        }
    }
    pub fn placeholder(self) -> &'static str {
        match self {
            CreateField::Name => "my-service",
            CreateField::Image => "docker.io/library/nginx:alpine",
            CreateField::Ports => "8080:80, 8443:443",
            CreateField::Env => "TZ=UTC, LOG_LEVEL=info",
            CreateField::Volumes => "pgdata:/var/lib/data",
            CreateField::Command => "leave blank for image default",
        }
    }
    /// Index of this field in [`Self::ALL`].
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|f| *f == self).unwrap_or(0)
    }
    pub const ALL: [CreateField; 6] = [
        CreateField::Name,
        CreateField::Image,
        CreateField::Ports,
        CreateField::Env,
        CreateField::Volumes,
        CreateField::Command,
    ];
    /// Number of create-form fields.
    pub const COUNT: usize = 6;
}

/// The new-container draft as the document holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateDraft {
    fields: [String; CreateField::COUNT],
}

impl CreateDraft {
    /// The draft with `values` filled in and every other field empty.
    pub fn with(values: &[(CreateField, &str)]) -> Self {
        let mut draft = Self::default();
        for (field, value) in values {
            if let Some(slot) = draft.fields.get_mut(field.index()) {
                (*value).clone_into(slot);
            }
        }
        draft
    }

    /// The text of `field`.
    pub fn get(&self, field: CreateField) -> &str {
        self.fields.get(field.index()).map_or("", String::as_str)
    }

    /// Whether nothing has been filled in.
    pub fn is_empty(&self) -> bool {
        self.fields.iter().all(|value| value.trim().is_empty())
    }

    /// The `podman run` argument vector the draft describes, or `None` while
    /// the image (the only required field) is empty.
    ///
    /// Translates the comma-separated Ports / Env / Volumes drafts into the
    /// matching `-p` / `-e` / `-v` flags, then the image, then any trailing
    /// command override. This is the single source of truth `submit_create`
    /// shells out with.
    pub fn run_args(&self) -> Option<Vec<String>> {
        let image = self.get(CreateField::Image).trim();
        if image.is_empty() {
            return None;
        }
        let mut args = vec!["run".to_string(), "-d".to_string()];
        let name = self.get(CreateField::Name).trim();
        if !name.is_empty() {
            args.push("--name".into());
            args.push(name.to_string());
        }
        for (flag, field) in [
            ("-p", CreateField::Ports),
            ("-e", CreateField::Env),
            ("-v", CreateField::Volumes),
        ] {
            for entry in split_list(self.get(field)) {
                args.push(flag.into());
                args.push(entry);
            }
        }
        args.push(image.to_string());
        args.extend(
            self.get(CreateField::Command)
                .split_whitespace()
                .map(ToOwned::to_owned),
        );
        Some(args)
    }
}

/// Reads and lays out the draft in the document. Writes happen only inside
/// the resource's actions.
pub struct CreateDoc;

impl CreateDoc {
    /// Lay out the draft: one map with an empty text per field, so every
    /// replica types into the same texts.
    ///
    /// # Errors
    /// Returns the document's refusal to create the objects.
    pub fn initialize(doc: &mut AutoCommit) -> Result<(), automerge::AutomergeError> {
        let map = match Self::map(doc) {
            Some(map) => map,
            None => doc.put_object(ROOT, CREATE_KEY, ObjType::Map)?,
        };
        for field in CreateField::ALL {
            if Self::text_in(doc, &map, field).is_none() {
                doc.put_object(&map, field.key(), ObjType::Text)?;
            }
        }
        Ok(())
    }

    /// The draft as stored; fields that are missing read as empty.
    pub fn read(doc: &AutoCommit) -> CreateDraft {
        let mut draft = CreateDraft::default();
        for field in CreateField::ALL {
            if let (Some(slot), Some(obj)) = (
                draft.fields.get_mut(field.index()),
                Self::field_obj(doc, field),
            ) {
                *slot = doc.text(&obj).unwrap_or_default();
            }
        }
        draft
    }

    /// The text object of `field`, if the draft has one.
    pub fn field_obj(doc: &AutoCommit, field: CreateField) -> Option<ObjId> {
        Self::map(doc).and_then(|map| Self::text_in(doc, &map, field))
    }

    /// The text object of `field`, laid out first when the draft does not
    /// have it yet. For the resource's actions only.
    ///
    /// # Errors
    /// Refuses when the document holds something else under the draft's
    /// keys.
    pub(crate) fn field_obj_mut(
        doc: &mut AutoCommit,
        field: CreateField,
    ) -> Result<ObjId, ActionError> {
        Self::initialize(doc)?;
        Self::field_obj(doc, field)
            .ok_or_else(|| ActionError::Refused(format!("the draft has no {} text", field.key())))
    }

    fn map(doc: &AutoCommit) -> Option<ObjId> {
        match doc.get(ROOT, CREATE_KEY) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => None,
        }
    }

    fn text_in(doc: &AutoCommit, map: &ObjId, field: CreateField) -> Option<ObjId> {
        match doc.get(map, field.key()) {
            Ok(Some((Value::Object(ObjType::Text), id))) => Some(id),
            _ => None,
        }
    }
}

/// Split a comma-separated draft into trimmed, non-empty entries.
fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_args_require_an_image_and_follow_the_draft() {
        assert!(
            CreateDraft::default().run_args().is_none(),
            "no image, no run"
        );

        let args = CreateDraft::with(&[
            (CreateField::Image, "nginx:alpine"),
            (CreateField::Name, "web"),
            (CreateField::Ports, "8080:80, 8443:443"),
        ])
        .run_args()
        .unwrap();
        assert_eq!(
            args,
            [
                "run",
                "-d",
                "--name",
                "web",
                "-p",
                "8080:80",
                "-p",
                "8443:443",
                "nginx:alpine"
            ]
        );

        let args = CreateDraft::with(&[
            (CreateField::Image, "postgres:16"),
            (CreateField::Env, "POSTGRES_DB=app, TZ=UTC"),
            (CreateField::Volumes, "pgdata:/var/lib/postgresql/data"),
            (CreateField::Command, "postgres -c fsync=off"),
        ])
        .run_args()
        .unwrap();
        assert_eq!(
            args,
            [
                "run",
                "-d",
                "-e",
                "POSTGRES_DB=app",
                "-e",
                "TZ=UTC",
                "-v",
                "pgdata:/var/lib/postgresql/data",
                "postgres:16",
                "postgres",
                "-c",
                "fsync=off"
            ]
        );
    }

    #[test]
    fn initialize_lays_out_one_text_per_field_once() {
        let mut doc = AutoCommit::new();
        CreateDoc::initialize(&mut doc).unwrap();
        let objects: Vec<_> = CreateField::ALL
            .into_iter()
            .map(|field| CreateDoc::field_obj(&doc, field).expect("a text per field"))
            .collect();
        CreateDoc::initialize(&mut doc).unwrap();
        for (field, object) in CreateField::ALL.into_iter().zip(objects) {
            assert_eq!(CreateDoc::field_obj(&doc, field), Some(object));
        }
        assert_eq!(CreateDoc::read(&doc), CreateDraft::default());
    }
}
