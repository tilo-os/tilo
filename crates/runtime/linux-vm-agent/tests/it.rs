// One test binary: each file under tests/ is a module of it.

#[cfg(target_os = "linux")]
#[path = "served.rs"]
pub mod served;

#[cfg(target_os = "linux")]
#[path = "agent_socket.rs"]
pub mod agent_socket;
