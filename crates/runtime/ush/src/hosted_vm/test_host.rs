//! A host that sells Linux VMs, for tests: everything a hosted session meets, with no
//! hypervisor and no payment provider.
//!
//! It answers the hosted-VM service's socket ([`legacy_vm::hosted::control`]) by admitting
//! each wake through the real plan guard ([`legacy_vm::hosted::plan::admit`]) against its
//! plans file, which sells a "Pro" plan. An account it does not name has no card on file,
//! and [`TestHost::add_card`] writes it into the plans file as the provider does. An
//! admitted program runs in a stand-in for the VM's agent that answers `ran <argv>` with
//! exit status 0.

#![allow(
    clippy::expect_used,
    reason = "a test fixture: a host it cannot set up is a test that failed"
)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use legacy_agent_protocol::link::AgentLink;
use legacy_agent_protocol::{
    AgentHello, ExitStatus, GuestMessage, GuestOs, HostMessage, PROTOCOL_MAJOR, recv, send,
};
use legacy_vm::hosted::control::{FRAMING, Reply, Request};
use legacy_vm::hosted::plan::{Plan, Plans, Sells, admit};
use legacy_vm::hosted::size::VmSize;
use tilo_actions::remote::host_account::{Limits, PlanOffer};

use super::HostedTarget;

pub const ADD_CARD: &str = "https://tilo.sh/billing/card?account={account}";
pub const MANAGE_BILLING: &str = "https://tilo.sh/billing?account={account}";

#[must_use]
pub fn selling() -> Plans {
    Plans {
        sells: Some(Sells {
            host: "Tilo Cloud".into(),
            plans: vec![PlanOffer {
                id: "pro".into(),
                name: "Pro".into(),
                price: Some("$20 a month, then $0.04 per VM hour".into()),
                limits: Limits {
                    max_running: 2,
                    max_vms: 5,
                    sizes: vec!["small".into(), "default".into()],
                    included_vm_seconds: Some(100 * 3600),
                    ..Limits::default()
                },
            }],
            add_payment_method: ADD_CARD.into(),
            manage_billing: Some(MANAGE_BILLING.into()),
            raise_limit: Some("https://tilo.sh/billing/plan?account={account}".into()),
            browser_free_actions: None,
        }),
        ..Plans::default()
    }
}

pub struct TestHost {
    dir: PathBuf,
    asked: Arc<Mutex<Vec<String>>>,
    full_for: Arc<Mutex<usize>>,
}

impl TestHost {
    #[must_use]
    pub fn start(dir: &Path) -> Self {
        let host = Self {
            dir: dir.to_path_buf(),
            asked: Arc::new(Mutex::new(Vec::new())),
            full_for: Arc::new(Mutex::new(0)),
        };
        host.write(&selling());
        let service = tilo_files::native::socket::bind(&host.socket()).expect("the service socket");
        let agent =
            tilo_files::native::socket::bind(&host.agent_socket()).expect("the agent socket");
        let (plans, agent_socket, asked, full_for) = (
            host.plans(),
            host.agent_socket(),
            Arc::clone(&host.asked),
            Arc::clone(&host.full_for),
        );
        std::thread::spawn(move || {
            for stream in service.incoming().flatten() {
                answer(stream, &plans, &agent_socket, &asked, &full_for);
            }
        });
        std::thread::spawn(move || {
            for stream in agent.incoming().flatten() {
                std::thread::spawn(move || run_agent(stream));
            }
        });
        host
    }

    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.dir.join("hosted.sock")
    }

    #[must_use]
    pub fn plans(&self) -> PathBuf {
        self.dir.join("plans.json")
    }

    fn agent_socket(&self) -> PathBuf {
        self.dir.join("agent.sock")
    }

    #[must_use]
    pub fn target(&self, account: &str) -> HostedTarget {
        HostedTarget {
            socket: self.socket(),
            account: account.into(),
            size: VmSize::Small,
            plans: Some(self.plans()),
        }
    }

    pub fn add_card(&self, account: &str) {
        let mut plans = Plans::load(&self.plans()).expect("the plans file");
        plans.accounts.insert(
            account.into(),
            Plan {
                max_running: 2,
                max_vms: 5,
                sizes: vec![VmSize::Small, VmSize::Default],
                plan: Some("pro".into()),
                ..Plan::default()
            },
        );
        self.write(&plans);
    }

    pub fn full_for(&self, wakes: usize) {
        *self.full_for.lock().unwrap_or_else(PoisonError::into_inner) = wakes;
    }

    #[must_use]
    pub fn wakes_asked(&self) -> usize {
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn write(&self, plans: &Plans) {
        tilo_files::native::atomic::write_creating_parent(
            &self.plans(),
            &serde_json::to_vec(plans).expect("plans encode"),
        )
        .expect("the plans file is written");
    }
}

fn answer(
    mut stream: UnixStream,
    plans: &Path,
    agent: &Path,
    asked: &Mutex<Vec<String>>,
    full_for: &Mutex<usize>,
) {
    let Ok(Some(request)) = FRAMING.read_value::<_, Request>(&mut stream) else {
        return;
    };
    let reply = match request {
        Request::Ensure { vm, .. } => {
            asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(vm.account.clone());
            let mut full = full_for.lock().unwrap_or_else(PoisonError::into_inner);
            if *full > 0 {
                *full = full.saturating_sub(1);
                let _ = FRAMING.write_value(
                    &mut stream,
                    &Reply::Busy(legacy_vm::hosted::service::BUSY.to_owned()),
                );
                return;
            }
            drop(full);
            match Plans::load(plans) {
                Err(error) => Reply::Error(error),
                Ok(plans) => {
                    let id = format!("{}/{}", vm.workspace, vm.environment);
                    match admit(&plans, &vm.account, &id, vm.size, &[], 0, false) {
                        Ok(()) => Reply::Ready {
                            link: AgentLink::Hybrid {
                                socket: agent.to_path_buf(),
                            },
                            woke_ms: Some(1),
                        },
                        Err(refusal) => {
                            Reply::Refused(refusal.typed(plans.sells.as_ref(), &vm.account))
                        }
                    }
                }
            }
        }
        other => Reply::Error(format!("the test host answers no {other:?}")),
    };
    let _ = FRAMING.write_value(&mut stream, &reply);
}

fn run_agent(mut stream: UnixStream) {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    while stream.read(&mut byte).is_ok_and(|read| read == 1) && byte != *b"\n" {
        line.extend_from_slice(&byte);
    }
    if !line.starts_with(b"CONNECT ") || stream.write_all(b"OK 1\n").is_err() {
        return;
    }
    let Ok(Some(HostMessage::Hello { .. })) = recv::<_, HostMessage>(&mut stream) else {
        return;
    };
    let hello = GuestMessage::Hello(AgentHello {
        protocol_major: PROTOCOL_MAJOR,
        os: GuestOs::Linux,
        boot_id: "test".into(),
        agent_version: "test".into(),
    });
    if send(&mut stream, &hello).is_err() {
        return;
    }
    let Ok(Some(HostMessage::Launch(launch))) = recv::<_, HostMessage>(&mut stream) else {
        return;
    };
    let _ = send(&mut stream, &GuestMessage::Started { pid: 1 });
    let _ = send(
        &mut stream,
        &GuestMessage::Stdout(format!("ran {}\n", launch.argv.join(" ")).into_bytes()),
    );
    let _ = send(
        &mut stream,
        &GuestMessage::Exited(ExitStatus {
            code: Some(0),
            signal: None,
        }),
    );
}
