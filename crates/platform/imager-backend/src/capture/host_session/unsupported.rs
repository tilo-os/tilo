use super::*;

impl HostSession {
    pub(super) fn spawn_native(_executable: &Path, _uid: u32, _output: bool) -> io::Result<Self> {
        Err(crate::capture::Error::Unsupported.into())
    }
    pub(super) fn process_native(&self) -> HostProcess {
        match self.never {}
    }
    pub(super) fn identity_native(&self) -> Incarnation {
        match self.never {}
    }
    pub(super) fn receipt_native(&self) -> ProjectionReceipt {
        match self.never {}
    }
    pub(super) fn handoff_native(
        &mut self,
        _source: &File,
        _output: Option<&File>,
        _scratch: (&File, u64),
        _request: &[u8],
        _operation: &str,
    ) -> io::Result<()> {
        match self.never {}
    }
    pub(super) fn events_native(&mut self) -> io::Result<Events> {
        match self.never {}
    }
    pub(super) fn acknowledge_native(&mut self) -> io::Result<()> {
        match self.never {}
    }
}
impl HostProcess {
    pub(super) fn revoke_native(&self) -> io::Result<()> {
        match self.never {}
    }
    pub(super) fn try_wait_native(&self) -> io::Result<Option<ExitStatus>> {
        match self.never {}
    }
    pub(super) fn wait_native(&self) -> io::Result<ExitStatus> {
        match self.never {}
    }
    pub(super) fn abort_native(&self) -> io::Result<()> {
        match self.never {}
    }
}
impl Events {
    pub(super) fn read_native(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        match self.never {}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsupported_host_never_starts_a_partial_session() {
        let error = HostSession::spawn(Path::new("not-executed"), 1000, false)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(matches!(
            error
                .get_ref()
                .unwrap()
                .downcast_ref::<crate::capture::Error>(),
            Some(crate::capture::Error::Unsupported)
        ));
    }
}
