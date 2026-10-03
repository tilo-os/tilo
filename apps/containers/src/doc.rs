//! CRDT-backed view state for the container manager.
//!
//! The *engine* snapshot (containers, images, stats) is transient data
//! mirrored from podman and lives in plain memory — it isn't collaborative,
//! it's a projection of the host. What *is* worth keeping in the shared
//! Automerge document is the **view state**: which section the user is in,
//! which container is selected, the active detail tab, the search query, and
//! the create-container form draft. Storing those in the CRDT means undo/redo
//! and live collaboration (two peers navigating the same manager) work for
//! free, exactly as the toolkit intends.
//!
//! ```text
//! root
//! └── "view" (Map)
//!     ├── "section"   (Str)   # "containers" | "images" | "volumes" | "distrobox"
//!     ├── "selected"  (Str)   # container id, or "" for none
//!     ├── "tab"       (Str)   # "logs" | "stats" | "inspect" | "terminal"
//!     ├── "search"    (Text)  # collaborative search box
//!     └── "create"    (Map)   # the new-container form draft
//!         ├── "name"    (Text)
//!         ├── "image"   (Text)
//!         ├── "ports"   (Text)
//!         ├── "env"     (Text)
//!         ├── "volumes" (Text)
//!         └── "command" (Text)
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

/// Which top-level section is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Containers,
    Images,
    Volumes,
    Distrobox,
}

impl Section {
    pub fn as_str(self) -> &'static str {
        match self {
            Section::Containers => "containers",
            Section::Images => "images",
            Section::Volumes => "volumes",
            Section::Distrobox => "distrobox",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "images" => Section::Images,
            "volumes" => Section::Volumes,
            "distrobox" => Section::Distrobox,
            _ => Section::Containers,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Section::Containers => "Containers",
            Section::Images => "Images",
            Section::Volumes => "Volumes",
            Section::Distrobox => "Distrobox",
        }
    }
}

/// Which tab of the container detail view is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailTab {
    Logs,
    Stats,
    Inspect,
    Terminal,
}

impl DetailTab {
    pub fn as_str(self) -> &'static str {
        match self {
            DetailTab::Logs => "logs",
            DetailTab::Stats => "stats",
            DetailTab::Inspect => "inspect",
            DetailTab::Terminal => "terminal",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "stats" => DetailTab::Stats,
            "inspect" => DetailTab::Inspect,
            "terminal" => DetailTab::Terminal,
            _ => DetailTab::Logs,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            DetailTab::Logs => "Logs",
            DetailTab::Stats => "Stats",
            DetailTab::Inspect => "Inspect",
            DetailTab::Terminal => "Terminal",
        }
    }
    pub const ALL: [DetailTab; 4] = [
        DetailTab::Logs,
        DetailTab::Stats,
        DetailTab::Inspect,
        DetailTab::Terminal,
    ];
}

/// Stateless helpers over the shared view-state document.
pub struct ViewDoc;

impl ViewDoc {
    fn single<'a>(
        doc: &'a AutoCommit,
        obj: &ObjId,
        key: &str,
    ) -> Result<Option<(Value<'a>, ObjId)>, StoredValueError> {
        let Ok(mut values) = doc.get_all(obj, key) else {
            return Err(StoredValueError::WrongType);
        };
        if values.len() > 1 {
            return Err(StoredValueError::Conflict);
        }
        Ok(values.pop())
    }

    fn existing_view_map(doc: &AutoCommit) -> Result<Option<ObjId>, StoredValueError> {
        match Self::single(doc, &ROOT, "view")? {
            None => Ok(None),
            Some((Value::Object(ObjType::Map), id)) => Ok(Some(id)),
            Some(_) => Err(StoredValueError::WrongType),
        }
    }

    fn view_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "view") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            // `put_object` on root only errs if root itself were deleted,
            // which can't happen; falling back to root keeps this from
            // panicking mid-collaboration if that assumption is ever wrong.
            _ => doc.put_object(&ROOT, "view", ObjType::Map).unwrap_or(ROOT),
        }
    }

    fn get_str(doc: &AutoCommit, map: &ObjId, key: &str) -> Option<String> {
        match Self::single(doc, map, key) {
            Ok(Some((Value::Scalar(s), _))) => s.as_str().map(ToOwned::to_owned),
            Ok(Some((Value::Object(ObjType::Text), tid))) => doc.text(&tid).ok(),
            Ok(None | Some(_)) | Err(_) => None,
        }
    }

    fn put_str(doc: &mut AutoCommit, map: &ObjId, key: &str, value: &str) {
        let _ = doc.put(map, key, value);
    }

    fn text_obj(doc: &mut AutoCommit, map: &ObjId, key: &str) -> ObjId {
        match doc.get(map, key) {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            // See `view_map`: fall back to the parent map rather than panic
            // on the practically-unreachable error path.
            _ => doc
                .put_object(map, key, ObjType::Text)
                .unwrap_or_else(|_| map.clone()),
        }
    }

    // --- section / selection / tab ---

    pub fn section(doc: &AutoCommit) -> Section {
        let value = Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, "section"));
        Section::parse(&value.unwrap_or_default())
    }

    pub fn set_section(doc: &mut AutoCommit, section: Section) {
        let m = Self::view_map(doc);
        Self::put_str(doc, &m, "section", section.as_str());
    }

    pub fn selected(doc: &AutoCommit) -> Option<String> {
        Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, "selected"))
            .filter(|s| !s.is_empty())
    }

    pub fn set_selected(doc: &mut AutoCommit, id: Option<&str>) {
        let m = Self::view_map(doc);
        Self::put_str(doc, &m, "selected", id.unwrap_or(""));
    }

    pub fn tab(doc: &AutoCommit) -> DetailTab {
        let value = Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, "tab"));
        DetailTab::parse(&value.unwrap_or_default())
    }

    pub fn set_tab(doc: &mut AutoCommit, tab: DetailTab) {
        let m = Self::view_map(doc);
        Self::put_str(doc, &m, "tab", tab.as_str());
    }

    // --- search (collaborative Text) ---

    pub fn search(doc: &AutoCommit) -> String {
        Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, "search"))
            .unwrap_or_default()
    }

    pub fn search_obj(doc: &mut AutoCommit) -> ObjId {
        let m = Self::view_map(doc);
        Self::text_obj(doc, &m, "search")
    }

    pub fn set_search(doc: &mut AutoCommit, value: &str) {
        let t = Self::search_obj(doc);
        ui::doc::splice_text_diffed(doc, &t, value);
    }

    // --- create-container draft ---

    fn existing_create_map(doc: &AutoCommit) -> Result<Option<ObjId>, StoredValueError> {
        let Some(view) = Self::existing_view_map(doc)? else {
            return Ok(None);
        };
        match Self::single(doc, &view, "create")? {
            None => Ok(None),
            Some((Value::Object(ObjType::Map), id)) => Ok(Some(id)),
            Some(_) => Err(StoredValueError::WrongType),
        }
    }

    fn create_map(doc: &mut AutoCommit) -> ObjId {
        let m = Self::view_map(doc);
        match doc.get(&m, "create") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            // See `view_map`: fall back to the parent map rather than panic
            // on the practically-unreachable error path.
            _ => doc.put_object(&m, "create", ObjType::Map).unwrap_or(m),
        }
    }

    /// `ObjId` of one of the create-form's Text fields, so a `TextInput`-style
    /// edit can splice directly into the shared draft.
    pub fn create_field_obj(doc: &AutoCommit, field: CreateField) -> Option<ObjId> {
        match Self::stored_create_field(doc, field) {
            StoredCreateField::Text { object, .. } => Some(object),
            StoredCreateField::Absent
            | StoredCreateField::WrongType
            | StoredCreateField::Conflict => None,
        }
    }

    pub fn stored_create_field(doc: &AutoCommit, field: CreateField) -> StoredCreateField {
        let map = match Self::existing_create_map(doc) {
            Ok(Some(map)) => map,
            Ok(None) => return StoredCreateField::Absent,
            Err(StoredValueError::WrongType) => return StoredCreateField::WrongType,
            Err(StoredValueError::Conflict) => return StoredCreateField::Conflict,
        };
        match Self::single(doc, &map, field.key()) {
            Ok(None) => StoredCreateField::Absent,
            Ok(Some((Value::Object(ObjType::Text), object))) => doc
                .text(&object)
                .map_or(StoredCreateField::WrongType, |value| {
                    StoredCreateField::Text { value, object }
                }),
            Ok(Some(_)) | Err(StoredValueError::WrongType) => StoredCreateField::WrongType,
            Err(StoredValueError::Conflict) => StoredCreateField::Conflict,
        }
    }

    pub fn create_field(doc: &AutoCommit, field: CreateField) -> String {
        match Self::stored_create_field(doc, field) {
            StoredCreateField::Text { value, .. } => value,
            StoredCreateField::Absent
            | StoredCreateField::WrongType
            | StoredCreateField::Conflict => String::new(),
        }
    }

    pub fn set_create_field(doc: &mut AutoCommit, field: CreateField, value: &str) {
        let map = Self::create_map(doc);
        let obj = Self::text_obj(doc, &map, field.key());
        ui::doc::splice_text_diffed(doc, &obj, value);
    }

    /// Reset every create-form field to empty.
    pub fn clear_create(doc: &mut AutoCommit) {
        if let Ok(Some(view)) = Self::existing_view_map(doc) {
            let _ = doc.delete(&view, "create");
        }
    }

    /// Assemble the `podman run` argument vector from the current draft.
    /// Returns `None` when the image field is empty (the only required one).
    ///
    /// Translates the comma-separated Ports / Env / Volumes drafts into the
    /// matching `-p` / `-e` / `-v` flags, then the image, then any trailing
    /// command override. This is the single source of truth the live
    /// `submit_create` shells out with — asserting on it (rather than a
    /// helper copy) proves the create flow builds the right podman call.
    pub fn create_run_args(doc: &AutoCommit) -> Option<Vec<String>> {
        let name = Self::create_field(doc, CreateField::Name);
        let image = Self::create_field(doc, CreateField::Image);
        let ports = Self::create_field(doc, CreateField::Ports);
        let env = Self::create_field(doc, CreateField::Env);
        let volumes = Self::create_field(doc, CreateField::Volumes);
        let command = Self::create_field(doc, CreateField::Command);
        if image.trim().is_empty() {
            return None;
        }
        let mut args = vec!["run".to_string(), "-d".to_string()];
        if !name.trim().is_empty() {
            args.push("--name".into());
            args.push(name.trim().to_string());
        }
        for mapping in split_list(&ports) {
            args.push("-p".into());
            args.push(mapping);
        }
        for kv in split_list(&env) {
            args.push("-e".into());
            args.push(kv);
        }
        for mount in split_list(&volumes) {
            args.push("-v".into());
            args.push(mount);
        }
        args.push(image.trim().to_string());
        for token in command.split_whitespace() {
            args.push(token.to_string());
        }
        Some(args)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoredValueError {
    WrongType,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredCreateField {
    Absent,
    Text { value: String, object: ObjId },
    WrongType,
    Conflict,
}

/// Split a comma-separated draft into trimmed, non-empty entries.
fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// The editable fields of the create-container form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Section, selection, tab, search and create-form state round-trip through the doc and build the run arguments.
    #[test]
    fn view_and_create_state_round_trip_through_doc() {
        // --- phase: section_round_trips_through_doc ---
        {
            let mut doc = AutoCommit::new();
            assert_eq!(ViewDoc::section(&doc), Section::Containers, "default");
            ViewDoc::set_section(&mut doc, Section::Images);
            assert_eq!(ViewDoc::section(&doc), Section::Images);
            ViewDoc::set_section(&mut doc, Section::Distrobox);
            assert_eq!(ViewDoc::section(&doc), Section::Distrobox);
        }

        // --- phase: selection_set_and_clear ---
        {
            let mut doc = AutoCommit::new();
            assert_eq!(ViewDoc::selected(&doc), None);
            ViewDoc::set_selected(&mut doc, Some("abc123"));
            assert_eq!(ViewDoc::selected(&doc), Some("abc123".into()));
            ViewDoc::set_selected(&mut doc, None);
            assert_eq!(ViewDoc::selected(&doc), None);
        }

        // --- phase: tab_round_trips ---
        {
            let mut doc = AutoCommit::new();
            assert_eq!(ViewDoc::tab(&doc), DetailTab::Logs, "default");
            ViewDoc::set_tab(&mut doc, DetailTab::Stats);
            assert_eq!(ViewDoc::tab(&doc), DetailTab::Stats);
        }

        // --- phase: search_text_round_trips ---
        {
            let mut doc = AutoCommit::new();
            ViewDoc::set_search(&mut doc, "nginx");
            assert_eq!(ViewDoc::search(&doc), "nginx");
            ViewDoc::set_search(&mut doc, "redis");
            assert_eq!(ViewDoc::search(&doc), "redis");
        }

        // --- phase: volumes_section_round_trips ---
        {
            let mut doc = AutoCommit::new();
            ViewDoc::set_section(&mut doc, Section::Volumes);
            assert_eq!(ViewDoc::section(&doc), Section::Volumes);
        }

        // --- phase: clear_create_empties_fields ---
        {
            let mut doc = AutoCommit::new();
            ViewDoc::set_create_field(&mut doc, CreateField::Image, "nginx");
            ViewDoc::clear_create(&mut doc);
            assert_eq!(ViewDoc::create_field(&doc, CreateField::Image), "");
        }

        // --- phase: create_run_args_requires_image ---
        {
            let mut doc = AutoCommit::new();
            assert!(
                ViewDoc::create_run_args(&doc).is_none(),
                "no image → no args"
            );
            ViewDoc::set_create_field(&mut doc, CreateField::Image, "nginx:alpine");
            ViewDoc::set_create_field(&mut doc, CreateField::Name, "web");
            ViewDoc::set_create_field(&mut doc, CreateField::Ports, "8080:80, 8443:443");
            let args = ViewDoc::create_run_args(&doc).unwrap();
            assert_eq!(args[0], "run");
            assert!(args.contains(&"--name".to_string()));
            assert!(args.contains(&"web".to_string()));
            assert!(args.contains(&"8080:80".to_string()));
            assert!(args.contains(&"8443:443".to_string()));
            assert!(args.contains(&"nginx:alpine".to_string()));
        }

        // --- phase: create_run_args_includes_env_and_volumes ---
        {
            let mut doc = AutoCommit::new();
            ViewDoc::set_create_field(&mut doc, CreateField::Image, "postgres:16");
            ViewDoc::set_create_field(&mut doc, CreateField::Env, "POSTGRES_DB=app, TZ=UTC");
            ViewDoc::set_create_field(
                &mut doc,
                CreateField::Volumes,
                "pgdata:/var/lib/postgresql/data",
            );
            let args = ViewDoc::create_run_args(&doc).unwrap();
            // -e flags for each env entry, -v for the volume mount.
            let e_count = args.iter().filter(|a| *a == "-e").count();
            assert_eq!(e_count, 2, "one -e per env entry: {args:?}");
            assert!(
                args.windows(2)
                    .any(|w| w[0] == "-e" && w[1] == "POSTGRES_DB=app")
            );
            assert!(args.windows(2).any(|w| w[0] == "-e" && w[1] == "TZ=UTC"));
            assert!(
                args.windows(2)
                    .any(|w| w[0] == "-v" && w[1] == "pgdata:/var/lib/postgresql/data")
            );
            // The image comes after every flag, before any command.
            let img_pos = args.iter().position(|a| a == "postgres:16").unwrap();
            let last_flag = args
                .iter()
                .rposition(|a| a == "-e" || a == "-v" || a == "-p")
                .unwrap();
            assert!(img_pos > last_flag, "image follows all flags: {args:?}");
        }
    }

    /// Appending to or repeatedly setting the search keeps the doc bounded.
    #[test]
    fn search_edits_keep_doc_bounded() {
        // --- phase: appending_one_char_to_search_is_bounded ---
        {
            // Anti-churn: growing a long search field by one char must splice only
            // the new char, not rewrite the whole value.
            let mut doc = AutoCommit::new();
            let long = "n".repeat(300);
            ViewDoc::set_search(&mut doc, &long);
            let before = doc.save().len();
            let mut longer = long.clone();
            longer.push('!');
            ViewDoc::set_search(&mut doc, &longer);
            let grew = doc.save().len() - before;
            assert_eq!(ViewDoc::search(&doc), longer, "value round-trips");
            assert!(grew < 40, "one-char search edit grew doc by {grew} bytes");
        }

        // --- phase: repeated_set_search_keeps_doc_bounded ---
        {
            let mut doc = AutoCommit::new();
            let long = "r".repeat(300);
            ViewDoc::set_search(&mut doc, &long);
            let a = doc.save().len();
            for _ in 0..50 {
                ViewDoc::set_search(&mut doc, &long);
            }
            let b = doc.save().len();
            assert_eq!(
                a, b,
                "re-storing the same search grew the document {a} -> {b}"
            );
        }
    }
}
