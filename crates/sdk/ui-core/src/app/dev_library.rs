// Load a newly built app library into the process that already owns the windows.
//
// The dev loop writes `reload.path` in the session directory after a successful
// build. Each open window keeps its id and its own state bytes. The library's
// `tilo_dev_apply` runs for every window, so the new code sees that window's
// state and no other. A library linked to the same `ui` can also publish a
// widget root and the action decoders its document edits go through; a second
// copy of `ui` is refused and the running root stays.

use std::cell::RefCell;
#[cfg(all(unix, any(feature = "native", test)))]
use std::ffi::CStr;

use crate::widget::AppWidget;

/// One window the process already has open.
#[cfg(any(feature = "native", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DevWindowSlot {
    pub id: String,
    /// The previous library's bytes for this window, or the checkpoint on the
    /// first reload. Passed to the new code as this window's state.
    pub state: Vec<u8>,
}

/// A view built by the new code, with the decoders of the actions it
/// publishes. They travel together: the view encodes its actions with the new
/// code, so only the new code's decoders can read them back.
pub struct DevRoot {
    pub view: Box<dyn AppWidget>,
    pub registry: crate::tilo_actions::ActionRegistry,
}

/// What a successful load produced. The caller installs `root` into the
/// windows it already has; a `None` root leaves those widgets alone.
#[cfg(any(feature = "native", test))]
pub(crate) struct DevReload {
    pub windows: Vec<DevWindowSlot>,
    pub root: Option<DevRoot>,
    /// `dlopen` and symbol lookup, before any of the library's code ran.
    pub load: std::time::Duration,
}

/// Address of this copy of `ui`. A library linked to the same copy returns
/// the same address; a second copy returns a different one and is not adopted.
pub fn dev_stamp_addr() -> *const () {
    dev_stamp_addr as *const ()
}

thread_local! {
    static DEV_ROOT: RefCell<Option<DevRoot>> = const { RefCell::new(None) };
    #[cfg(all(unix, any(feature = "native", test)))]
    static LOADED_LIBS: RefCell<Vec<*mut libc::c_void>> = const { RefCell::new(Vec::new()) };
}

/// Store a widget root and its action decoders, built by the library that
/// shares this `ui`.
pub fn publish_dev_root(root: DevRoot) {
    DEV_ROOT.with(|slot| *slot.borrow_mut() = Some(root));
}

/// Take the root [`publish_dev_root`] stored on this thread.
pub fn take_dev_root() -> Option<DevRoot> {
    DEV_ROOT.with(|slot| slot.borrow_mut().take())
}

#[cfg(any(feature = "native", test))]
type ApplyFn = unsafe extern "C" fn(*const u8, usize, *const u8, usize, *mut u8, usize) -> isize;
#[cfg(any(feature = "native", test))]
type LinkedFn = unsafe extern "C" fn() -> *const ();
#[cfg(any(feature = "native", test))]
type PublishFn = unsafe extern "C" fn() -> i32;

#[cfg(any(feature = "native", test))]
struct DevFns {
    apply: ApplyFn,
    linked_ui: Option<LinkedFn>,
    publish: Option<PublishFn>,
}

/// Load `path` and run it against `windows`.
///
/// On `Err`, the caller keeps every window as it was. A loaded library stays
/// mapped: the widgets it built point into it.
#[cfg(all(unix, any(feature = "native", test)))]
pub(crate) fn reload_from_library(
    path: &Path,
    windows: &[DevWindowSlot],
) -> Result<DevReload, String> {
    let started = crate::time::now();
    let bytes = path.as_os_str().as_encoded_bytes();
    let path =
        std::ffi::CString::new(bytes).map_err(|_| "library path contains a nul".to_owned())?;
    // SAFETY: `path` is a C string. `RTLD_NOW` reports a missing symbol now
    // rather than on the first call. The handle is closed on failure and kept
    // on success.
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        return Err(last_dlerror("opening the reload library"));
    }
    let Some(apply) = (unsafe { load_symbol::<ApplyFn>(handle, b"tilo_dev_apply\0") }) else {
        unsafe { libc::dlclose(handle) };
        return Err("reload library has no tilo_dev_apply".to_owned());
    };
    let linked_ui = unsafe { load_symbol::<LinkedFn>(handle, b"tilo_dev_linked_ui\0") };
    let publish = unsafe { load_symbol::<PublishFn>(handle, b"tilo_dev_publish_root\0") };
    let load = started.elapsed();
    match reload_with(
        &DevFns {
            apply,
            linked_ui,
            publish,
        },
        windows,
    ) {
        Ok(reloaded) => {
            LOADED_LIBS.with(|libs| libs.borrow_mut().push(handle));
            Ok(DevReload { load, ..reloaded })
        }
        Err(error) => {
            unsafe { libc::dlclose(handle) };
            Err(error)
        }
    }
}

#[cfg(all(not(unix), any(feature = "native", test)))]
pub(crate) fn reload_from_library(
    _path: &Path,
    _windows: &[DevWindowSlot],
) -> Result<DevReload, String> {
    Err("in-place reload loads a shared library on unix".to_owned())
}

#[cfg(any(feature = "native", test))]
struct ReloadSchemas {
    accepted: bool,
}

#[cfg(any(feature = "native", test))]
impl Drop for ReloadSchemas {
    fn drop(&mut self) {
        if !self.accepted {
            tilo_resource::reset_type_schemas();
        }
    }
}

#[cfg(any(feature = "native", test))]
fn reload_with(fns: &DevFns, windows: &[DevWindowSlot]) -> Result<DevReload, String> {
    let mut root = None;
    let mut schemas = None;
    if let (Some(linked_ui), Some(publish)) = (fns.linked_ui, fns.publish) {
        // SAFETY: the symbol is the library's `tilo_dev_linked_ui`, which
        // returns the `ui` it linked and does not dereference caller state.
        let linked = unsafe { linked_ui() };
        if linked != dev_stamp_addr() {
            return Err(
                "reload library is linked to a different ui; keeping the running app".to_owned(),
            );
        }
        // Nominal resource types keep their IDs across app-code reloads.
        tilo_resource::reset_type_schemas();
        schemas = Some(ReloadSchemas { accepted: false });
        let _ = take_dev_root();
        // SAFETY: the symbol is the library's `tilo_dev_publish_root`. It
        // builds a root into this process's `ui` slot and returns 1, or
        // returns another value and leaves the slot empty.
        if unsafe { publish() } != 1 {
            return Err("reload library did not publish a view".to_owned());
        }
        root = Some(take_dev_root().ok_or_else(|| {
            "reload library published a view into a different ui; keeping the running app"
                .to_owned()
        })?);
    }

    let mut next = Vec::with_capacity(windows.len());
    for window in windows {
        let mut out = vec![0u8; window.state.len().saturating_add(65_536)];
        let id = window.id.as_bytes();
        // SAFETY: `id` and `state` are this window's bytes. `out` is the
        // buffer the symbol writes into. A negative return rejects the reload.
        let written = unsafe {
            (fns.apply)(
                id.as_ptr(),
                id.len(),
                window.state.as_ptr(),
                window.state.len(),
                out.as_mut_ptr(),
                out.len(),
            )
        };
        if written < 0 {
            return Err(format!("reload of window {} failed", window.id));
        }
        let written = usize::try_from(written).map_err(|_| {
            format!(
                "reload of window {} reported a length that does not fit",
                window.id
            )
        })?;
        if written > out.len() {
            return Err(format!(
                "reload of window {} wrote past its buffer",
                window.id
            ));
        }
        out.truncate(written);
        next.push(DevWindowSlot {
            id: window.id.clone(),
            state: out,
        });
    }
    if let Some(schemas) = schemas.as_mut() {
        schemas.accepted = true;
    }
    Ok(DevReload {
        windows: next,
        root,
        load: std::time::Duration::ZERO,
    })
}

/// # Safety
/// `handle` is an open `dlopen` handle. `name` is a nul-terminated C string.
#[cfg(all(unix, any(feature = "native", test)))]
unsafe fn load_symbol<T>(handle: *mut libc::c_void, name: &[u8]) -> Option<T> {
    let found = unsafe { libc::dlsym(handle, name.as_ptr().cast()) };
    if found.is_null() {
        return None;
    }
    if std::mem::size_of::<T>() != std::mem::size_of::<*mut libc::c_void>() {
        return None;
    }
    // SAFETY: `T` is a function pointer the same width as `dlsym`'s result,
    // and `name` is the symbol the caller asked for.
    Some(unsafe { std::mem::transmute_copy(&found) })
}

#[cfg(all(unix, any(feature = "native", test)))]
fn last_dlerror(what: &str) -> String {
    let err = unsafe { libc::dlerror() };
    if err.is_null() {
        format!("{what} failed")
    } else {
        format!(
            "{what}: {}",
            unsafe { CStr::from_ptr(err) }.to_string_lossy()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DevFns, DevWindowSlot, dev_stamp_addr, publish_dev_root, reload_from_library, reload_with,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    fn windows() -> Vec<DevWindowSlot> {
        vec![
            DevWindowSlot {
                id: "primary".to_owned(),
                state: b"hello".to_vec(),
            },
            DevWindowSlot {
                id: "notes".to_owned(),
                state: b"draft".to_vec(),
            },
        ]
    }

    fn rustc_cdylib(dir: &std::path::Path, name: &str, source: &str) -> std::path::PathBuf {
        let source_path = dir.join(format!("{name}.rs"));
        std::fs::write(&source_path, source).unwrap();
        let output = dir.join(format!("lib{name}.so"));
        let status = std::process::Command::new("rustc")
            .args(["--edition", "2024", "--crate-type", "cdylib", "-o"])
            .arg(&output)
            .arg(&source_path)
            .status()
            .expect("rustc runs");
        assert!(status.success(), "rustc {name} exited {status}");
        output
    }

    fn apply_library(tag: &str) -> String {
        format!(
            r#"
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tilo_dev_apply(
    id: *const u8,
    id_len: usize,
    state: *const u8,
    state_len: usize,
    out: *mut u8,
    out_cap: usize,
) -> isize {{
    let id = unsafe {{ std::slice::from_raw_parts(id, id_len) }};
    let state = if state.is_null() {{
        &[][..]
    }} else {{
        unsafe {{ std::slice::from_raw_parts(state, state_len) }}
    }};
    let mut next = state.to_vec();
    next.extend_from_slice(b"|");
    next.extend_from_slice(id);
    next.extend_from_slice(b"|{tag}");
    if next.len() > out_cap {{
        return -1;
    }}
    unsafe {{ std::ptr::copy_nonoverlapping(next.as_ptr(), out, next.len()); }}
    next.len() as isize
}}
"#
        )
    }

    #[test]
    #[cfg(unix)]
    fn dev_library_runs_new_code_in_every_open_window() {
        let dir = std::env::temp_dir().join(format!("tilo-dev-lib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let open = windows();
        let first = rustc_cdylib(&dir, "v1", &apply_library("v1"));
        let reloaded = reload_from_library(&first, &open).expect("v1 loads");
        assert!(
            reloaded.root.is_none(),
            "a state library does not replace the view"
        );
        assert_eq!(
            reloaded
                .windows
                .iter()
                .map(|window| window.id.as_str())
                .collect::<Vec<_>>(),
            vec!["primary", "notes"]
        );
        assert_eq!(reloaded.windows[0].state, b"hello|primary|v1");
        assert_eq!(reloaded.windows[1].state, b"draft|notes|v1");

        let second = rustc_cdylib(&dir, "v2", &apply_library("v2"));
        let again = reload_from_library(&second, &reloaded.windows).expect("v2 loads");
        assert_eq!(again.windows[0].state, b"hello|primary|v1|primary|v2");
        assert_eq!(again.windows[1].state, b"draft|notes|v1|notes|v2");
        assert_eq!(open[0].state, b"hello");
        assert_eq!(open[1].state, b"draft");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn dev_library_failure_leaves_every_window_unchanged() {
        let dir = std::env::temp_dir().join(format!("tilo-dev-lib-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = r#"
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tilo_dev_apply(
    id: *const u8,
    id_len: usize,
    _state: *const u8,
    _state_len: usize,
    _out: *mut u8,
    _out_cap: usize,
) -> isize {
    let id = unsafe { std::slice::from_raw_parts(id, id_len) };
    if id == b"notes" { -1 } else { 0 }
}
"#;
        let library = rustc_cdylib(&dir, "fail", source);
        let open = windows();
        let Err(error) = reload_from_library(&library, &open) else {
            panic!("notes rejects the reload");
        };
        assert!(error.contains("notes"), "{error}");
        assert_eq!(open[0].state, b"hello");
        assert_eq!(open[1].state, b"draft");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn dev_library_rejects_a_second_copy_of_ui() {
        let dir = std::env::temp_dir().join(format!("tilo-dev-lib-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = r#"
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tilo_dev_apply(
    _id: *const u8,
    _id_len: usize,
    _state: *const u8,
    _state_len: usize,
    _out: *mut u8,
    _out_cap: usize,
) -> isize {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn tilo_dev_linked_ui() -> *const () {
    1 as *const ()
}
#[unsafe(no_mangle)]
pub extern "C" fn tilo_dev_publish_root() -> i32 {
    1
}
"#;
        let library = rustc_cdylib(&dir, "otherui", source);
        let open = windows();
        let Err(error) = reload_from_library(&library, &open) else {
            panic!("a second ui is refused");
        };
        assert!(error.contains("different ui"), "{error}");
        assert_eq!(open[0].state, b"hello");
        assert_eq!(open[1].state, b"draft");
        let _ = std::fs::remove_dir_all(&dir);
    }

    static FAIL_NOTES: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn probe_apply(
        id: *const u8,
        id_len: usize,
        state: *const u8,
        state_len: usize,
        out: *mut u8,
        out_cap: usize,
    ) -> isize {
        let id = unsafe { std::slice::from_raw_parts(id, id_len) };
        if FAIL_NOTES.load(Ordering::Relaxed) && id == b"notes" {
            return -1;
        }
        let state = unsafe { std::slice::from_raw_parts(state, state_len) };
        let mut next = state.to_vec();
        next.extend_from_slice(b"|");
        next.extend_from_slice(id);
        if next.len() > out_cap {
            return -1;
        }
        unsafe { std::ptr::copy_nonoverlapping(next.as_ptr(), out, next.len()) };
        isize::try_from(next.len()).unwrap_or(-1)
    }

    unsafe extern "C" fn probe_linked() -> *const () {
        dev_stamp_addr()
    }

    unsafe extern "C" fn probe_publish() -> i32 {
        let mut registry = crate::tilo_actions::ActionRegistry::new();
        registry.register::<ReloadedEdit>("reloaded.edit");
        publish_dev_root(super::DevRoot {
            view: Box::new(ReloadedProbe),
            registry,
        });
        1
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct ReloadedEdit;

    impl crate::tilo_actions::Action for ReloadedEdit {
        fn apply(
            &self,
            _doc: &mut crate::tilo_actions::Doc,
        ) -> Result<(), crate::tilo_actions::ActionError> {
            Ok(())
        }
    }

    struct ReloadedProbe;

    impl crate::widget::Widget for ReloadedProbe {
        crate::testing::inert_widget_methods!(leaf_layout, name = "reloaded");
    }

    impl crate::widget::AppWidget for ReloadedProbe {
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Editor
        }

        fn app_title(&self) -> &str {
            "reloaded"
        }

        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Named("reloaded".into())
        }
    }

    thread_local! {
        static SCHEMA_REVISION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static PUBLISHED_SCHEMA: std::cell::RefCell<Option<crate::tilo_actions::Resource>> = const { std::cell::RefCell::new(None) };
    }

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    struct SchemaEdit {
        value: u8,
    }

    impl crate::tilo_actions::Action for SchemaEdit {
        fn apply(
            &self,
            _doc: &mut crate::tilo_actions::Doc,
        ) -> Result<(), crate::tilo_actions::ActionError> {
            Ok(())
        }
    }

    struct SchemaReload;

    impl tilo_resource::Resource for SchemaReload {
        const KIND: &'static str = "test.schema-reload";
        const LABEL: &'static str = "Schema reload";
        type State = ();
        type Action = SchemaEdit;

        fn read(_doc: &tilo_resource::Doc) -> Result<(), tilo_resource::ModelError> {
            Ok(())
        }

        fn action_schema() -> Option<serde_json::Value> {
            let maximum = SCHEMA_REVISION.with(|revision| if revision.get() { 255 } else { 15 });
            Some(
                serde_json::json!({"type":"object", "properties":{"value":{"type":"integer", "maximum":maximum}}, "required":["value"]}),
            )
        }
    }

    fn published_schema(
        resource: &crate::tilo_actions::Resource,
        action: &str,
    ) -> serde_json::Value {
        resource
            .actions
            .iter()
            .find(|spec| spec.id == action)
            .expect("generic operation")
            .params()
            .first()
            .expect("operation argument")
            .schema
            .as_deref()
            .cloned()
            .expect("typed schema")
    }

    unsafe extern "C" fn publish_schema_revision() -> i32 {
        let resource = tilo_resource::publish::<SchemaReload>("schema", "Schema", &());
        PUBLISHED_SCHEMA.with(|slot| *slot.borrow_mut() = Some(resource));
        let mut registry = crate::tilo_actions::ActionRegistry::new();
        registry.register::<SchemaEdit>("test.schema-reload");
        publish_dev_root(super::DevRoot {
            view: Box::new(ReloadedProbe),
            registry,
        });
        1
    }

    unsafe extern "C" fn reject_schema_revision() -> i32 {
        let resource = tilo_resource::publish::<SchemaReload>("schema", "Schema", &());
        PUBLISHED_SCHEMA.with(|slot| *slot.borrow_mut() = Some(resource));
        0
    }

    #[test]
    fn code_reload_refreshes_resource_schemas_before_publishing_the_view() {
        SCHEMA_REVISION.with(|revision| revision.set(false));
        let before = tilo_resource::publish::<SchemaReload>("schema", "Schema", &());
        assert_eq!(
            published_schema(&before, tilo_resource::APPLY)["properties"]["value"]["maximum"],
            15
        );
        let grouped_edit = |value| {
            let arguments = serde_json::json!({
                tilo_resource::GROUP_PARAM: [{
                    "action": tilo_resource::APPLY,
                    "args": {tilo_resource::APPLY_PARAM: {"value": value}},
                }],
            });
            tilo_resource::group::build_group::<SchemaReload>(arguments.as_object().unwrap(), 0)
        };
        assert!(grouped_edit(15).is_ok());
        assert!(
            grouped_edit(16).is_err(),
            "grouped edits enforce the old limit"
        );
        // Stand in for the new library's definition, with the same nominal resource type.
        SCHEMA_REVISION.with(|revision| revision.set(true));
        let reloaded = reload_with(
            &DevFns {
                apply: probe_apply,
                linked_ui: Some(probe_linked),
                publish: Some(publish_schema_revision),
            },
            &[],
        )
        .expect("matching UI reloads");
        assert!(
            reloaded
                .root
                .expect("new view")
                .registry
                .contains("test.schema-reload")
        );
        let after = PUBLISHED_SCHEMA.with(|slot| {
            slot.borrow_mut()
                .take()
                .expect("new code published its resource")
        });
        assert_eq!(
            published_schema(&after, tilo_resource::APPLY)["properties"]["value"]["maximum"],
            255,
            "agents see the reloaded edit's current argument limits"
        );
        assert_eq!(
            published_schema(&after, tilo_resource::GROUP)
                .pointer("/items/oneOf/0/properties/args/properties/action/description"),
            Some(&serde_json::json!(tilo_resource::resource::APPLY_MEMBER)),
            "grouped agent edits refer to the current apply schema"
        );
        assert!(
            grouped_edit(16).is_ok(),
            "grouped edits no longer use the old limit"
        );
        assert!(grouped_edit(255).is_ok(), "grouped edits use the new limit");
        assert!(
            grouped_edit(256).is_err(),
            "grouped edits enforce the new limit"
        );
        SCHEMA_REVISION.with(|revision| revision.set(false));
        let rejected = reload_with(
            &DevFns {
                apply: probe_apply,
                linked_ui: Some(probe_linked),
                publish: Some(reject_schema_revision),
            },
            &[],
        );
        assert!(
            rejected.is_err(),
            "the candidate failed to publish its view"
        );
        // The accepted code still runs after the failed replacement.
        SCHEMA_REVISION.with(|revision| revision.set(true));
        let retained = tilo_resource::publish::<SchemaReload>("schema", "Schema", &());
        assert_eq!(
            published_schema(&retained, tilo_resource::APPLY)["properties"]["value"]["maximum"],
            255,
            "failed code cannot leave its schemas in the retained view"
        );
    }

    #[test]
    fn matching_ui_installs_the_new_view_for_every_window() {
        FAIL_NOTES.store(false, Ordering::Relaxed);
        let open = windows();
        let reloaded = reload_with(
            &DevFns {
                apply: probe_apply,
                linked_ui: Some(probe_linked),
                publish: Some(probe_publish),
            },
            &open,
        )
        .expect("matching ui reloads");
        let root = reloaded.root.expect("the new view is published");
        assert_eq!(root.view.name(), "reloaded");
        assert!(
            root.registry.contains("reloaded.edit"),
            "the new code's decoders come with its view"
        );
        assert_eq!(reloaded.windows[0].id, "primary");
        assert_eq!(reloaded.windows[1].id, "notes");
        assert_eq!(reloaded.windows[0].state, b"hello|primary");
        assert_eq!(reloaded.windows[1].state, b"draft|notes");

        FAIL_NOTES.store(true, Ordering::Relaxed);
        let Err(error) = reload_with(
            &DevFns {
                apply: probe_apply,
                linked_ui: Some(probe_linked),
                publish: Some(probe_publish),
            },
            &open,
        ) else {
            panic!("one window rejects the reload");
        };
        assert!(error.contains("notes"), "{error}");
        assert_eq!(open[0].state, b"hello");
        assert_eq!(open[1].state, b"draft");
    }
}
