use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_process_operations_preserve_typed_unsupported() {
        let host: Incarnation = serde_json::from_value(serde_json::json!({
            "pid": 2, "start": 1, "uid": 1000, "boot": vec![1; 16]
        }))
        .unwrap();
        let mut ownership = Ownership {
            host,
            namespace: None,
            launch_acknowledged: false,
        };
        let namespace = Namespace {
            init_pid: 3,
            start_ticks: 1,
            namespace_dev: 1,
            namespace_ino: 1,
        };
        let errors = [
            Incarnation::capture(2, 1000).unwrap_err(),
            ownership.active_namespace().unwrap_err(),
            ownership.admit_namespace(namespace).unwrap_err(),
            ownership.require_stopped().unwrap_err(),
            ownership.stop_and_observe().unwrap_err(),
        ];
        for error in errors {
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
}

impl Incarnation {
    pub fn capture(_pid: u32, _uid: u32) -> io::Result<Self> {
        Err(super::super::Error::Unsupported.into())
    }
}
impl Ownership {
    pub fn active_namespace(&self) -> io::Result<bool> {
        Err(super::super::Error::Unsupported.into())
    }
    pub fn admit_namespace(&mut self, _namespace: Namespace) -> io::Result<()> {
        Err(super::super::Error::Unsupported.into())
    }
    pub fn require_stopped(&self) -> io::Result<()> {
        Err(super::super::Error::Unsupported.into())
    }
    pub fn stop_and_observe(&self) -> io::Result<()> {
        Err(super::super::Error::Unsupported.into())
    }
}
