//! The workspace an app is launched into.
//!
//! A workspace is a collaboration workspace: its id is the cluster id every
//! change an app makes is addressed to, and what the person granted apps in it
//! is what the launcher lets them reach. Both travel with the launch — the id
//! as `--workspace-id`, and the grants as a JSON file under the per-user
//! runtime directory that the launcher reads and the app never sees
//! ([`Workspace::write_grants`]).
//!
//! An app launched with no workspace gets the manifest's sandbox and nothing
//! else; that is the safe default, so a workspace is only ever passed when the
//! desktop knows which workspace the launch belongs to.

use std::path::{Path, PathBuf};

use tilo_action_policy::Grantee;
use tilo_permissions::caps::CapOverride;
use tilo_permissions::caps::DeviceCap;
use tilo_permissions::caps::Effect;
use tilo_permissions::caps::LaunchGrants;
use tilo_permissions::caps::Scope;

/// One workspace, as a launch into it needs it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workspace {
    /// The workspace's id, or empty for a launch outside any workspace.
    pub id: String,
    /// What the person granted apps in this workspace.
    pub grants: LaunchGrants,
    /// The person at this device, by the id the workspace's governance names
    /// them by: whose grants are minted here.
    pub person: String,
    /// Where the session keeps a launch's private files, under the per-user
    /// runtime directory.
    pub directory: PathBuf,
    /// The devices of this machine the person allowed each app, as
    /// `(app, device)`: what their own answers give, never the workspace's.
    pub devices: Vec<(String, DeviceCap)>,
}

/// What a package declares for the network, before anyone here answered for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredReach {
    /// The hosts it names, `*` when it may ask for any.
    pub hosts: Vec<String>,
    /// It asks for calls: UDP audio and video while a call of it is live.
    pub calls: bool,
}

impl Workspace {
    /// A workspace with no grants: an app gets exactly what its manifest
    /// declares.
    #[must_use]
    pub fn new(id: impl Into<String>, directory: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            grants: LaunchGrants::default(),
            person: String::new(),
            directory: directory.into(),
            devices: Vec::new(),
        }
    }

    /// The workspace apps are launched into by default: `workspace`'s id, the
    /// launcher's capability directory under `runtime`.
    #[must_use]
    pub fn under(runtime: &Path, id: impl Into<String>) -> Self {
        Self::new(id, runtime.join("tilo").join("capabilities"))
    }

    /// Whether a launch into this workspace names a workspace at all.
    #[must_use]
    pub fn is_workspace(&self) -> bool {
        !self.id.is_empty()
    }

    /// What a launch of `app` is granted: this workspace's grants for it.
    ///
    /// Only what the workspace's grants hand this app reaches it — a
    /// capability naming a person, a workspace or another app never does
    /// ([`LaunchGrants::for_app`]). No app is handed a folder of the device it
    /// runs on by standing: Files shows the workspace's own tree through the
    /// workspace's files resource, and a folder on this device only once the
    /// person picks it, which is a choice kept for that app alone.
    #[must_use]
    pub fn grants_for_launch(&self, app: &str) -> LaunchGrants {
        let mut grants = self.grants.for_app(app);
        // A workspace only ever refuses the person's devices; their own answers allow them.
        grants.sandbox.retain(|cap| {
            !(cap.effect == Effect::Grant
                && matches!(cap.scope, Scope::Device(device) if device.is_personal()))
        });
        for (_, device) in self.devices.iter().filter(|(allowed, _)| allowed == app) {
            grants.sandbox.push(
                CapOverride::grant(
                    Grantee::person(self.person.clone()),
                    Scope::Device(*device),
                    device.access(),
                )
                .through(app),
            );
        }
        grants
    }

    /// The network `app` declares that nobody in this workspace has answered for yet: the hosts
    /// its manifest names, or `*` when it may ask for any, and whether it asks for calls. The
    /// launcher lets an app reach only what both its manifest and a network grant here allow, so
    /// the person is asked for these before it starts. `None` for an app the image ships, which
    /// reaches what it declares; for one that declares no network; and once any network grant or
    /// restriction here names the app.
    #[must_use]
    pub fn unanswered_reach(
        &self,
        app: &str,
        net: &tilo_app_package::manifest::SandboxNet,
        image_shipped: bool,
    ) -> Option<DeclaredReach> {
        use tilo_app_package::manifest::NetMode;
        let (hosts, calls) = match net.mode {
            NetMode::Allowlist => (net.hosts.clone(), net.calls),
            NetMode::Host => (vec!["*".to_owned()], net.calls),
            _ => (Vec::new(), net.calls),
        };
        if hosts.is_empty() && !calls {
            return None;
        }
        (!image_shipped && !self.answers_reach(app)).then_some(DeclaredReach { hosts, calls })
    }

    /// Whether anyone in this workspace answered what `app` may reach: a network grant or
    /// restriction here names it.
    #[must_use]
    pub fn answers_reach(&self, app: &str) -> bool {
        self.grants.sandbox.iter().any(|cap| {
            cap.names_app(app) && matches!(cap.scope, Scope::NetHost(_) | Scope::NetCalls)
        })
    }

    /// Write the grants where `tilo-launcher --capabilities` reads them, and
    /// return that path: `None` when nothing was granted, so the launch
    /// carries no capability flag at all.
    ///
    /// The file is created owner-only before anything is written to it, and
    /// is left for the launcher to read rather than deleted here: a launch
    /// that fails to start must not take the file with it, and the runtime
    /// directory does not outlive the session.
    ///
    /// # Errors
    /// The directory or the file could not be written.
    pub fn write_grants(&self, app: &str) -> std::io::Result<Option<PathBuf>> {
        launcher::capabilities::write_capability_file(
            &self.directory,
            app,
            &self.grants_for_launch(app),
        )
    }
}

#[cfg(test)]
mod tests {
    use tilo_app_package::manifest::{NetMode, SandboxNet};

    use super::*;
    use tilo_permissions::caps::Access;
    use tilo_permissions::caps::CapOverride;
    use tilo_permissions::caps::Scope;

    fn granted(workspace: &Workspace, app: &str, path: &str) -> LaunchGrants {
        let mut grants = workspace.grants.clone();
        grants.sandbox.push(
            CapOverride::grant(
                Grantee::person("person-7"),
                Scope::FsPath(path.into()),
                Access::Read,
            )
            .through(app),
        );
        grants
    }

    fn read(path: &Path) -> LaunchGrants {
        serde_json::from_slice(&std::fs::read(path).expect("readable")).expect("JSON")
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one flow through a launch's grants, which is what it proves"
    )]
    fn a_launch_into_a_workspace_is_handed_its_grants_and_nothing_else_it_was_not_given() {
        let directory = tempfile::tempdir().expect("a directory");
        let mut workspace = Workspace::under(directory.path(), "workspace-7");
        assert_eq!(workspace.id, "workspace-7");
        assert!(workspace.is_workspace());
        assert_eq!(
            workspace.write_grants("so.tilo.notes").expect("written"),
            None,
            "nothing granted, no capability file and no flag"
        );

        // No app is handed this machine's home by standing, Files included: a folder on the
        // device is the person's to pick, and Files shows the workspace's own tree otherwise.
        workspace.person = "person-7".to_owned();
        assert!(
            workspace
                .grants_for_launch("so.tilo.file-explorer")
                .is_empty(),
            "Files is handed no folder of this device by standing"
        );
        assert!(
            workspace.grants.sandbox.is_empty(),
            "the workspace's synced record never names this machine's home"
        );
        assert!(
            workspace.grants_for_launch("so.tilo.notes").is_empty(),
            "no other app browses the device's files"
        );

        // What was granted is written where the launcher reads it, owner-only,
        // and reads back as it was.
        workspace.grants = granted(&workspace, "so.tilo.notes", "/home/you/notes");
        let path = workspace
            .write_grants("so.tilo.notes")
            .expect("written")
            .expect("a capability file");
        assert!(path.starts_with(directory.path().join("tilo").join("capabilities")));
        assert_eq!(
            read(&path),
            workspace.grants,
            "the launcher reads back what was granted"
        );
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(&path)
                .expect("the file")
                .permissions()
                .mode()
        };
        assert_eq!(mode & 0o777, 0o600, "the person's grants are owner-only");

        // An installed app's declared network is asked for until a grant names
        // it, and an app the image ships is asked nothing.
        let maps = SandboxNet {
            mode: NetMode::Allowlist,
            hosts: vec!["tile.openstreetmap.org".into()],
            calls: false,
        };
        assert_eq!(
            workspace.unanswered_reach("so.example.maps", &maps, false),
            Some(DeclaredReach {
                hosts: vec!["tile.openstreetmap.org".to_owned()],
                calls: false,
            })
        );
        assert_eq!(
            workspace.unanswered_reach("so.example.maps", &maps, true),
            None
        );
        let off = SandboxNet {
            mode: NetMode::Off,
            hosts: Vec::new(),
            calls: false,
        };
        assert_eq!(
            workspace.unanswered_reach("so.example.maps", &off, false),
            None
        );
        let any = SandboxNet {
            mode: NetMode::Host,
            hosts: Vec::new(),
            calls: false,
        };
        assert_eq!(
            workspace.unanswered_reach("so.example.mail", &any, false),
            Some(DeclaredReach {
                hosts: vec!["*".to_owned()],
                calls: false,
            })
        );
        // A calling app with no TCP host is asked for its calls, and a calls grant answers it.
        let calls = SandboxNet {
            mode: NetMode::Off,
            hosts: Vec::new(),
            calls: true,
        };
        assert_eq!(
            workspace.unanswered_reach("so.example.discord", &calls, false),
            Some(DeclaredReach {
                hosts: Vec::new(),
                calls: true,
            })
        );
        // Another app's grant answers nothing for this one; its own does.
        workspace.grants.sandbox.push(
            CapOverride::grant(
                Grantee::person("person-7"),
                Scope::NetHost("tile.openstreetmap.org".into()),
                Access::Read,
            )
            .through("so.example.other"),
        );
        assert!(
            workspace
                .unanswered_reach("so.example.maps", &maps, false)
                .is_some()
        );
        workspace.grants.sandbox.push(
            CapOverride::grant(
                Grantee::person("person-7"),
                Scope::NetHost("tile.openstreetmap.org".into()),
                Access::Read,
            )
            .through("so.example.maps"),
        );
        assert_eq!(
            workspace.unanswered_reach("so.example.maps", &maps, false),
            None
        );

        // A launch reads back only what this app was granted, not the
        // workspace's other grants.
        workspace.grants = {
            let mut grants = workspace.grants.clone();
            grants.sandbox.push(
                CapOverride::grant(
                    Grantee::person("person-7"),
                    Scope::FsPath("/home/you/mail".into()),
                    Access::Read,
                )
                .through("so.tilo.mail"),
            );
            grants
        };
        let path = workspace
            .write_grants("so.tilo.notes")
            .expect("written")
            .expect("a capability file");
        let filtered = read(&path);
        assert_eq!(
            filtered,
            workspace.grants_for_launch("so.tilo.notes"),
            "the launcher reads what was granted to this app, not the workspace's other grants"
        );
        assert_eq!(filtered.sandbox.len(), 1);

        // An app id that could leave the launch's directory is refused, and a
        // launch outside a workspace names none.
        let directory = tempfile::tempdir().expect("a directory");
        let mut workspace = Workspace::under(directory.path(), "workspace");
        workspace.grants = granted(&workspace, "so.tilo.notes", "/home/you/notes");
        for app in ["", "../escape", "so.tilo/notes"] {
            assert!(
                workspace.write_grants(app).is_err(),
                "{app:?} must not name a path"
            );
        }
        assert!(
            !Workspace::under(directory.path(), "").is_workspace(),
            "a launch outside a workspace names no workspace"
        );

        // Every launch path writes what it is granted where the launcher reads it, and a launch
        // granted nothing writes no file at all: Files opens on the workspace's own tree here,
        // whatever home this machine runs as.
        assert_eq!(
            workspace
                .write_grants("so.tilo.file-explorer")
                .expect("written"),
            None,
            "Files is handed no folder of this device, so no capability file is written"
        );
        assert_eq!(
            workspace.grants_for_launch("so.tilo.file-explorer"),
            LaunchGrants::default(),
            "the launcher is handed only what the workspace granted"
        );
    }
}
