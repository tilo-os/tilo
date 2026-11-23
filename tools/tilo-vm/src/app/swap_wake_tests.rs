use super::*;

struct Guest {
    state: &'static str,
    fail_wake: bool,
    probe_known: bool,
    log: PathBuf,
}

impl Runner for Guest {
    fn output(&mut self, command: &CommandLine) -> Result<hypervisor::Output, CliError> {
        use std::io::Write as _;
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .unwrap();
        let probe = command.args.iter().any(|arg| arg == "domstate");
        let wake = command.args.iter().any(|arg| arg == "dompmwakeup");
        if !probe && !wake {
            return Err(CliError::InvalidInput(format!(
                "unexpected power operation {command:?}"
            )));
        }
        writeln!(log, "{}", if probe { "domstate" } else { "dompmwakeup" }).unwrap();
        let success = if probe {
            self.probe_known
        } else {
            !self.fail_wake
        };
        Ok(hypervisor::Output {
            success,
            status: format!("exit status: {}", u8::from(!success)),
            stdout: if probe {
                self.state.into()
            } else {
                String::new()
            },
            stderr: if success {
                String::new()
            } else {
                "fixture refusal".into()
            },
        })
    }

    fn interactive(&mut self, _: &CommandLine) -> Result<bool, CliError> {
        Err(CliError::InvalidInput(
            "wake policy must not open an interactive command".into(),
        ))
    }
}

fn guest(root: &Path, state: &'static str) -> Guest {
    Guest {
        state,
        fail_wake: false,
        probe_known: true,
        log: root.join("calls"),
    }
}

tilo_test_env::scenario! {
    fn a_swap_wakes_only_a_managed_guest_that_sleeps;

    phase a_sleeping_guest_is_woken_before_its_device_is_asked;
    {
        let dir = tempfile::tempdir().unwrap();
        let mut guest = guest(dir.path(), "pmsuspended");
        let entry = VmEntry {
            hypervisor: "libvirt".into(),
            ..VmEntry::default()
        };
        let vm = Vm {
            name: "test",
            entry: &entry,
        };
        let adapter = hypervisor::adapter(&entry.hypervisor).unwrap();
        let log = guest.log.clone();
        let device = crate::remote::fixture::Device::serving(move |action, params| {
            assert_eq!(
                std::fs::read_to_string(&log).unwrap(),
                "domstate\ndompmwakeup\n",
                "the guest was woken before its device was asked"
            );
            assert_eq!(action, "swap");
            assert_eq!(params["account"], "test-account");
            Ok(serde_json::json!({
                "account": "test-account", "generation": 8, "changed": false,
                "policy": "no_restart", "actions": [],
            }))
        });
        let mut remote = swap_transport(adapter.as_ref(), &vm, &mut guest, || {
            Remote::connect("test", &device.machine, &device.route)
        })
        .unwrap();
        let applied = remote
            .call(Swap {
                account: "test-account".into(),
                component: "shell".into(),
                layer: "a".repeat(64),
                info: None,
                partner: None,
            })
            .unwrap();
        assert_eq!(applied.generation, 8);
        assert!(!applied.changed);
        assert_eq!(device.asked().len(), 1);
    }

    phase wake_failure_stops_before_transport;
    {
        let dir = tempfile::tempdir().unwrap();
        let mut guest = guest(dir.path(), "pmsuspended");
        guest.fail_wake = true;
        let entry = VmEntry {
            hypervisor: "libvirt".into(),
            ..VmEntry::default()
        };
        let vm = Vm {
            name: "test",
            entry: &entry,
        };
        let adapter = hypervisor::adapter(&entry.hypervisor).unwrap();
        let called = std::cell::Cell::new(false);
        let result = swap_transport(adapter.as_ref(), &vm, &mut guest, || {
            called.set(true);
            Ok(())
        });
        assert!(matches!(result, Err(CliError::CommandFailed { .. })));
        assert!(!called.get());
        assert_eq!(
            std::fs::read_to_string(&guest.log).unwrap(),
            "domstate\ndompmwakeup\n"
        );
    }

    phase physical_swaps_never_probe_or_change_power_state;
    {
        let dir = tempfile::tempdir().unwrap();
        let mut guest = guest(dir.path(), "pmsuspended");
        let entry = VmEntry {
            hypervisor: hypervisor::DEVICE.into(),
            ..VmEntry::default()
        };
        let vm = Vm {
            name: "physical",
            entry: &entry,
        };
        let adapter = hypervisor::adapter(&entry.hypervisor).unwrap();
        let result: Result<(), CliError> = swap_transport(adapter.as_ref(), &vm, &mut guest, || {
            Err(CliError::InvalidInput("physical transport refused".into()))
        });
        assert!(matches!(result, Err(CliError::InvalidInput(_))));
        assert!(!guest.log.exists(), "physical power adapter must not run");
    }

    phase running_off_and_unknown_managed_states_do_not_start_or_wake;
    {
        for (state, known) in [
            ("running", true),
            ("shut off", true),
            ("pmsuspended", false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut guest = guest(dir.path(), state);
            guest.probe_known = known;
            let entry = VmEntry {
                hypervisor: "libvirt".into(),
                ..VmEntry::default()
            };
            let vm = Vm {
                name: "test",
                entry: &entry,
            };
            let adapter = hypervisor::adapter(&entry.hypervisor).unwrap();
            let called = std::cell::Cell::new(false);
            swap_transport(adapter.as_ref(), &vm, &mut guest, || {
                called.set(true);
                Ok(())
            })
            .unwrap();
            assert!(called.get());
            assert_eq!(std::fs::read_to_string(&guest.log).unwrap(), "domstate\n");
        }
    }
}
