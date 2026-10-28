//! Explicit access actor; source custody lasts until the controlling pipe closes.
use super::*;
struct Access {
    grants: Grants,
    source: migration_handoff::RetainedSource,
}
impl Access {
    fn review(grants: Grants, mode: migration_handoff::AccessMode) -> io::Result<Self> {
        let plan = Plan::read_private(&grants.plans, &grants.operation)?;
        if !plan.categories.home {
            return Err(failure());
        }
        let source = migration_handoff::retain_access(&plan, mode)?;
        imager_backend::setup::deferred_handoff::import_selected_from_source(
            &plan,
            source.path(),
            &grants.home,
            &grants.workspace,
            super::current_person(),
            &std::sync::atomic::AtomicBool::new(false),
        )?;
        let result = Self { grants, source };
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> io::Result<()> {
        for path in [
            &self.grants.plans,
            &self.grants.home,
            &self.grants.workspace,
        ] {
            private(path)?;
            self.source.validate_target(path)?;
        }
        let current = Plan::read_private(&self.grants.plans, &self.grants.operation)?;
        self.source.validate_plan(&current)?;
        if !current.categories.home {
            return Err(failure());
        }
        self.source.validate()
    }
    fn status(&self) -> io::Result<serde_json::Value> {
        use sha2::{Digest as _, Sha256};
        let plan = Plan::read_private(&self.grants.plans, &self.grants.operation)?;
        self.source.validate_plan(&plan)?;
        let digest = hex::encode(Sha256::digest(plan.bytes()?));
        Ok(
            serde_json::json!({"event":"existing-files-available","plan_sha256":digest,"operation":self.grants.operation,"read_only":self.source.mode()==migration_handoff::AccessMode::ReadOnly,"access_mode":self.source.mode(),
            "source":super::super::migration_store::NativePath::encode(self.source.path()),"source_display":self.source.path().to_string_lossy(),"copied_bytes":0}),
        )
    }
}
pub(super) fn run(
    grants: Grants,
    mode: migration_handoff::AccessMode,
    input: impl io::Read + Send + 'static,
    output: &mut impl Write,
) -> io::Result<()> {
    let access = Access::review(grants, mode)?;
    emit(output, &access.status()?)?;
    let (sender, receiver) = mpsc::sync_channel(8);
    std::thread::spawn(move || lines(input, sender));
    loop {
        match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(command)) if command.op == "revalidate" => {
                access.validate()?;
                emit(output, &access.status()?)?;
            }
            Ok(Ok(command)) if command.op == "release" => return Ok(()),
            Ok(_) => {
                emit(
                    output,
                    &serde_json::json!({"event":"action-unavailable","operation":access.grants.operation,"access_mode":access.source.mode()}),
                )?;
                return Err(failure());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => access.validate()?,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}
