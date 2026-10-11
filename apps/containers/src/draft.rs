//! The container manager's record: the new-container draft.
//!
//! The engine (containers, images, volumes, distroboxes, their stats and
//! logs) is this device's podman, mirrored into memory and never written
//! here; where the user is in the app (section, selection, detail tab) is
//! the window's own. What the record holds is what a person fills in and
//! may finish on another device or with someone else: the create form, one
//! collaborative text per field, edited at its path
//! (`Containers::paths().draft.image`).

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Model, Path};

use crate::resource::Containers;

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
    pub fn placeholder(self) -> &'static str {
        match self {
            CreateField::Name => "my-service",
            CreateField::Image => "docker.io/library/nginx:alpine",
            CreateField::Ports => "8080:80, 8443:443",
            CreateField::Env => "TZ=UTC, LOG_LEVEL=info",
            CreateField::Volumes => "pgdata:/var/lib/postgresql/data",
            CreateField::Command => "",
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
    /// Where the field's text is in the record.
    pub fn path(self) -> Path<Containers, String> {
        let draft = Containers::paths().draft;
        match self {
            CreateField::Name => draft.name,
            CreateField::Image => draft.image,
            CreateField::Ports => draft.ports,
            CreateField::Env => draft.env,
            CreateField::Volumes => draft.volumes,
            CreateField::Command => draft.command,
        }
    }
}

/// The new-container draft: one text per field, each merging per
/// character with a second person typing in the same form.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateDraft {
    /// Name
    pub name: String,
    /// Image
    pub image: String,
    /// Ports
    pub ports: String,
    /// Environment
    pub env: String,
    /// Volumes
    pub volumes: String,
    /// Command
    pub command: String,
}

impl CreateDraft {
    /// The draft with `values` filled in and every other field empty.
    pub fn with(values: &[(CreateField, &str)]) -> Self {
        let mut draft = Self::default();
        for (field, value) in values {
            (*value).clone_into(draft.field_mut(*field));
        }
        draft
    }

    /// The text of `field`.
    pub fn get(&self, field: CreateField) -> &str {
        match field {
            CreateField::Name => &self.name,
            CreateField::Image => &self.image,
            CreateField::Ports => &self.ports,
            CreateField::Env => &self.env,
            CreateField::Volumes => &self.volumes,
            CreateField::Command => &self.command,
        }
    }

    fn field_mut(&mut self, field: CreateField) -> &mut String {
        match field {
            CreateField::Name => &mut self.name,
            CreateField::Image => &mut self.image,
            CreateField::Ports => &mut self.ports,
            CreateField::Env => &mut self.env,
            CreateField::Volumes => &mut self.volumes,
            CreateField::Command => &mut self.command,
        }
    }

    /// Whether nothing has been filled in.
    pub fn is_empty(&self) -> bool {
        CreateField::ALL
            .iter()
            .all(|field| self.get(*field).trim().is_empty())
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
}
