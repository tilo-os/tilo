//! The page-visible form of a store key: how `IndexedDB` rows name what they
//! hold.

use durable_store::StorageKey;

/// The page-visible form of [`StorageKey`].
///
/// Written as derived; read by [`DiskKey::read`], which takes the same JSON.
/// A derived reader of an internally tagged enum buffers the whole value and
/// is compiled for it alone, a large share of the code a page loads first
/// for a handful of strings.
#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum DiskKey {
    Document {
        app: String,
        workspace: String,
        document: String,
    },
    App {
        app: String,
    },
    WorkspaceIndex {
        app: String,
        workspace: String,
    },
    View {
        device: String,
        workspace: String,
        instance: u64,
    },
}

impl From<&StorageKey> for DiskKey {
    fn from(key: &StorageKey) -> Self {
        match key.clone() {
            StorageKey::Document {
                app,
                workspace,
                document,
            } => Self::Document {
                app: app.to_string(),
                workspace,
                document,
            },
            StorageKey::App { app } => Self::App {
                app: app.to_string(),
            },
            StorageKey::WorkspaceIndex { app, workspace } => Self::WorkspaceIndex {
                app: app.to_string(),
                workspace,
            },
            StorageKey::View {
                device,
                workspace,
                instance,
            } => Self::View {
                device,
                workspace,
                instance,
            },
        }
    }
}

impl DiskKey {
    /// The key `text` names, as written: `{"type": .., ..fields}`, with
    /// exactly the fields its type has.
    pub(crate) fn read(text: &str) -> Result<Self, String> {
        let serde_json::Value::Object(fields) =
            serde_json::from_slice::<serde_json::Value>(text.as_bytes())
                .map_err(|error| error.to_string())?
        else {
            return Err("a stored key is not an object".into());
        };
        let kind = fields
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or("a stored key names no type")?;
        let names: &[&str] = match kind {
            "document" => &["app", "workspace", "document"],
            "app" => &["app"],
            "workspaceIndex" => &["app", "workspace"],
            "view" => &["device", "workspace", "instance"],
            other => return Err(format!("unknown stored key type `{other}`")),
        };
        if let Some(extra) = fields
            .keys()
            .find(|name| *name != "type" && !names.contains(&name.as_str()))
        {
            return Err(format!("unknown field `{extra}` in a `{kind}` key"));
        }
        let text = |name: &str| {
            fields
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("a `{kind}` key has no `{name}`"))
        };
        Ok(match kind {
            "document" => Self::Document {
                app: text("app")?,
                workspace: text("workspace")?,
                document: text("document")?,
            },
            "app" => Self::App { app: text("app")? },
            "workspaceIndex" => Self::WorkspaceIndex {
                app: text("app")?,
                workspace: text("workspace")?,
            },
            _ => Self::View {
                device: text("device")?,
                workspace: text("workspace")?,
                instance: fields
                    .get("instance")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("a `view` key has no `instance`")?,
            },
        })
    }

    /// The store key this row holds.
    pub(crate) fn into_key(self) -> StorageKey {
        match self {
            Self::Document {
                app,
                workspace,
                document,
            } => StorageKey::Document {
                app: app.into(),
                workspace,
                document,
            },
            Self::App { app } => StorageKey::App { app: app.into() },
            Self::WorkspaceIndex { app, workspace } => StorageKey::WorkspaceIndex {
                app: app.into(),
                workspace,
            },
            Self::View {
                device,
                workspace,
                instance,
            } => StorageKey::View {
                device,
                workspace,
                instance,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DiskKey;
    use durable_store::StorageKey;

    fn keys() -> Vec<StorageKey> {
        vec![
            StorageKey::Document {
                app: "so.tilo.notes".into(),
                workspace: "w".into(),
                document: "r".into(),
            },
            StorageKey::App {
                app: "so.tilo.notes".into(),
            },
            StorageKey::WorkspaceIndex {
                app: "so.tilo.notes".into(),
                workspace: "w".into(),
            },
            StorageKey::View {
                device: "d".into(),
                workspace: "w".into(),
                instance: 7,
            },
        ]
    }

    #[test]
    fn every_key_reads_back_as_written() {
        for key in keys() {
            let written = serde_json::to_string(&DiskKey::from(&key)).unwrap();
            let read = DiskKey::read(&written).unwrap().into_key();
            assert_eq!(read, key, "{written}");
        }
    }

    #[test]
    fn a_malformed_key_is_refused() {
        for refused in [
            r#"{"type":"app","app":"p","document":"r"}"#,
            r#"{"type":"document","app":"p","workspace":"w"}"#,
            r#"{"type":"view","device":"d","workspace":"w","instance":"7"}"#,
            r#"{"type":"elsewhere","app":"p"}"#,
            r#"{"app":"p"}"#,
            r#"["app","p"]"#,
        ] {
            assert!(DiskKey::read(refused).is_err(), "{refused}");
        }
    }
}
