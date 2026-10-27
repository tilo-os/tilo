//! Private owner-selected file contents on the existing authenticated chooser connection.
use super::{ChooserContext, PickerScope, PortalCaller};
use crate::portal::{FilePickerRequest, PickerFilter};
use crate::shell::state::WorkspaceAuthority as _;
use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tilo_dialogs::shell::{
    CONTENT_CHUNK_BYTES, FilePortalGrant, MAX_CONTENT_BYTES, MAX_CONTENT_FILES, Reply, Request,
};

const LEASE: Duration = Duration::from_secs(60);
const CHOOSE_DEADLINE: Duration = Duration::from_secs(300);
const MAX_PENDING: usize = 4;
/// How long the lease thread of a connection holding no file sleeps before it looks
/// whether the connection is gone: a closed one wakes it at once, a dropped one does not.
const IDLE_RECHECK: Duration = Duration::from_secs(300);

#[derive(Default)]
pub(super) struct ConnectionGrants {
    closed: AtomicBool,
    state: Mutex<State>,
    /// Wakes [`ConnectionGrants::maintain`]'s thread when a file is granted or the
    /// connection closes; otherwise it sleeps until the next lease ends.
    changed: Condvar,
}
#[derive(Default)]
struct State {
    files: HashMap<String, Selected>,
    pending: usize,
}
struct SelectionRoot {
    path: std::path::PathBuf,
    directory: File,
}
fn anchor_scope(scope: &PickerScope) -> Result<Vec<SelectionRoot>, String> {
    scope
        .roots
        .iter()
        .map(|path| {
            Ok(SelectionRoot {
                path: path.clone(),
                directory: crate::shell::nearby_transfer::SecureNearbySource::anchor_directory(
                    path,
                )?,
            })
        })
        .collect()
}
struct Selected {
    grant: FilePortalGrant,
    file: File,
    stamp: Stamp,
    offset: u64,
    expires: Instant,
}
#[derive(PartialEq, Eq)]
struct Stamp(u64, u64, u64, i64, i64, i64, i64);
impl Stamp {
    fn of(meta: &Metadata) -> Self {
        Self(
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
        )
    }
}
impl ConnectionGrants {
    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Ok(mut state) = self.state.lock() {
            state.files.clear();
        }
        self.changed.notify_all();
    }
    /// Release each selected file when its lease ends. The thread sleeps until the
    /// earliest lease ends, or, holding none, until a file is granted or the connection
    /// closes: an open app with nothing chosen costs no wakeups.
    pub(super) fn maintain(self: &Arc<Self>) -> std::io::Result<()> {
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("tilo-picker-leases".into())
            .spawn(move || {
                loop {
                    let Some(connection) = weak.upgrade() else {
                        break;
                    };
                    if connection.closed.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(mut state) = connection.state.lock() else {
                        break;
                    };
                    let now = Instant::now();
                    state.files.retain(|_, file| file.expires > now);
                    // The connection is held only while waiting, so a dropped one ends
                    // the thread at its next wake; closing wakes it at once.
                    let next = state.files.values().map(|file| file.expires).min();
                    let woken = match next {
                        Some(expires) => connection
                            .changed
                            .wait_timeout(state, expires.saturating_duration_since(now))
                            .map(|(state, _)| state),
                        None => connection
                            .changed
                            .wait_timeout(state, IDLE_RECHECK)
                            .map(|(state, _)| state),
                    };
                    if woken.is_err() {
                        break;
                    }
                }
            })
            .map(|_| ())
    }
}
fn refused(id: u64) -> Reply {
    Reply::Failed {
        id,
        reason: crate::shell::i18n::t("portal-chooser-file-grant-unavailable"),
    }
}
pub(super) fn owner(context: &ChooserContext, caller: &PortalCaller) -> bool {
    let handle = context.grants.tree_handle();
    let tree = handle.read();
    tree.desktop.active_workspace().is_some_and(|workspace| {
        workspace.id == caller.workspace_id
            && tree.governs_room(workspace.room())
            && workspace.may_administer(&crate::shell::state::local_user_id_for_room(
                &workspace.room(),
            ))
    })
}

pub(super) fn answer(
    context: &ChooserContext,
    caller: &PortalCaller,
    request: Request,
    connection: &Arc<ConnectionGrants>,
) -> Reply {
    match request {
        Request::PickContents {
            id,
            multiple,
            title,
            accept_label,
            filters,
        } => {
            if connection.closed.load(Ordering::Acquire) || !owner(context, caller) {
                return refused(id);
            }
            if !(context.presence)() {
                return Reply::Nobody { id };
            }
            {
                let Ok(mut state) = connection.state.lock() else {
                    return refused(id);
                };
                if state.pending >= MAX_PENDING {
                    return refused(id);
                }
                state.pending = state.pending.saturating_add(1);
            }
            let result = choose(
                context,
                caller,
                connection,
                Pick {
                    id,
                    multiple,
                    title,
                    accept_label,
                    filters,
                },
            );
            if let Ok(mut state) = connection.state.lock() {
                state.pending = state.pending.saturating_sub(1);
            }
            result
        }
        Request::ReadContents {
            id,
            grant,
            offset,
            max_bytes,
        } => {
            if connection.closed.load(Ordering::Acquire) || !owner(context, caller) {
                return refused(id);
            }
            let Ok(mut state) = connection.state.lock() else {
                return refused(id);
            };
            let Some(selected) = state.files.get_mut(&grant.token) else {
                return refused(id);
            };
            let result = read(selected, &grant, offset, max_bytes);
            if let Some((bytes, eof)) = result {
                if eof {
                    state.files.remove(&grant.token);
                }
                Reply::ContentsChunk { id, bytes, eof }
            } else {
                state.files.remove(&grant.token);
                refused(id)
            }
        }
        Request::ReleaseContents { id, grant } => {
            if let Ok(mut state) = connection.state.lock()
                && state
                    .files
                    .get(&grant.token)
                    .is_some_and(|selected| selected.grant == grant)
            {
                state.files.remove(&grant.token);
            }
            Reply::ContentsReleased { id }
        }
        _ => refused(0),
    }
}
struct Pick {
    id: u64,
    multiple: bool,
    title: String,
    accept_label: String,
    filters: Vec<tilo_dialogs::shell::Filter>,
}
fn choose(
    context: &ChooserContext,
    caller: &PortalCaller,
    connection: &ConnectionGrants,
    pick: Pick,
) -> Reply {
    // The host owns this reach. It authorizes no directory for the app.
    let scope = PickerScope::for_user().with_granted_paths(
        context
            .grants
            .granted_paths(caller)
            .into_iter()
            .filter(|path| {
                context.grants.filesystem_path_allows(
                    caller,
                    path,
                    collab_types::caps::Access::Read,
                )
            }),
    );
    let Ok(roots) = anchor_scope(&scope) else {
        return refused(pick.id);
    };
    let (reply, mut chosen) = tokio::sync::oneshot::channel();
    context.pickers.spawn(FilePickerRequest {
        caller: caller.clone(),
        parent_window: None,
        title: pick.title,
        save: false,
        directory: false,
        writable_directory: false,
        multiple: pick.multiple,
        accept_label: Some(pick.accept_label),
        current_name: None,
        filters: pick
            .filters
            .iter()
            .map(|filter| {
                let pairs = filter
                    .globs
                    .iter()
                    .map(|glob| (0, glob.clone()))
                    .chain(filter.mimetypes.iter().map(|mime| (1, mime.clone())))
                    .collect::<Vec<_>>();
                PickerFilter::from_pairs(filter.name.clone(), &pairs)
            })
            .collect(),
        scope: Some(scope),
        recent: Vec::new(),
        reply,
    });
    (context.wake)();
    let Some(deadline) = Instant::now().checked_add(CHOOSE_DEADLINE) else {
        return refused(pick.id);
    };
    let paths = loop {
        if connection.closed.load(Ordering::Acquire)
            || !owner(context, caller)
            || Instant::now() >= deadline
        {
            return refused(pick.id);
        }
        match chosen.try_recv() {
            Ok(Some(paths)) => break paths,
            Ok(None) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                return Reply::Cancelled { id: pick.id };
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    grant_paths(connection, &roots, pick.id, pick.multiple, paths)
}
fn grant_paths(
    connection: &ConnectionGrants,
    roots: &[SelectionRoot],
    id: u64,
    multiple: bool,
    paths: Vec<std::path::PathBuf>,
) -> Reply {
    if paths.is_empty() || paths.len() > MAX_CONTENT_FILES || (!multiple && paths.len() != 1) {
        return refused(id);
    }
    let mut selected = Vec::new();
    let mut total = 0_u64;
    for path in paths {
        let source = roots.iter().find_map(|root| {
            crate::shell::nearby_transfer::SecureNearbySource::open_in_directory(
                &root.directory,
                &root.path,
                &path,
            )
            .ok()
        });
        let Some(source) = source else {
            return refused(id);
        };
        let Ok(meta) = source.metadata() else {
            return refused(id);
        };
        total = total.saturating_add(meta.len());
        if !meta.is_file() || total > MAX_CONTENT_BYTES {
            return refused(id);
        }
        let grant = FilePortalGrant {
            request: id,
            token: uuid::Uuid::new_v4().to_string(),
            name: source.name().to_owned(),
            size: meta.len(),
        };
        selected.push(Selected {
            grant,
            file: source.into_file(),
            stamp: Stamp::of(&meta),
            offset: 0,
            expires: match Instant::now().checked_add(LEASE) {
                Some(expires) => expires,
                None => return refused(id),
            },
        });
    }
    let Ok(mut state) = connection.state.lock() else {
        return refused(id);
    };
    if connection.closed.load(Ordering::Acquire)
        || state.files.len().saturating_add(selected.len()) > MAX_CONTENT_FILES
    {
        return refused(id);
    }
    let grants = selected.iter().map(|file| file.grant.clone()).collect();
    for file in selected {
        state.files.insert(file.grant.token.clone(), file);
    }
    drop(state);
    connection.changed.notify_all();
    Reply::ContentsGranted { id, grants }
}
fn read(
    selected: &mut Selected,
    grant: &FilePortalGrant,
    offset: u64,
    max_bytes: u32,
) -> Option<(Vec<u8>, bool)> {
    if &selected.grant != grant
        || selected.expires <= Instant::now()
        || selected.offset != offset
        || max_bytes == 0
        || max_bytes > CONTENT_CHUNK_BYTES
        || selected.stamp != Stamp::of(&selected.file.metadata().ok()?)
    {
        return None;
    }
    let remaining = grant.size.checked_sub(offset)?;
    let len = usize::try_from(remaining.min(u64::from(max_bytes))).ok()?;
    let mut bytes = vec![0; len];
    selected.file.read_exact(&mut bytes).ok()?;
    if selected.stamp != Stamp::of(&selected.file.metadata().ok()?) {
        return None;
    }
    selected.offset = selected.offset.checked_add(len as u64)?;
    Some((bytes, selected.offset == grant.size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_and_closed_contents_grants_release_selected_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("selected.tapp");
        std::fs::write(&path, b"actual selected bytes").unwrap();
        let scope = PickerScope {
            initial_dir: root.path().into(),
            roots: vec![root.path().into()],
            home: None,
        };
        let connection = ConnectionGrants::default();
        let roots = anchor_scope(&scope).unwrap();
        let Reply::ContentsGranted { grants, .. } =
            grant_paths(&connection, &roots, 1, false, vec![path])
        else {
            panic!("production selection must open a regular readonly file")
        };
        let grant = grants.into_iter().next().unwrap();
        {
            let mut state = connection.state.lock().unwrap();
            let selected = state.files.get_mut(&grant.token).unwrap();
            selected.expires = Instant::now();
            let refused = read(selected, &grant, 0, CONTENT_CHUNK_BYTES).is_none();
            drop(state);
            assert!(refused, "expired grant must never return bytes");
        }
        connection.close();
        assert!(connection.closed.load(Ordering::Acquire));
        assert!(
            connection.state.lock().unwrap().files.is_empty(),
            "disconnect drops every held readonly file"
        );
        assert!(matches!(
            grant_paths(&connection, &roots, 2, false, Vec::new()),
            Reply::Failed { .. }
        ));
    }
}
