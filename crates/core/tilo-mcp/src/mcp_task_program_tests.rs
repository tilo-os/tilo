//! A task's program, started by a listing that asks what the apps declare: what a listing does
//! when that program never answers.

use super::*;

/// A task's program that starts but never answers: its connection is a socket pair whose far
/// end this test keeps open and silent, so a read on it waits as a program that stopped does.
struct SilentProgram {
    /// The far end of every connection started, held open and never written.
    held: Mutex<Vec<tilo_socket::Stream>>,
}

impl crate::TaskHosts for SilentProgram {
    fn start(&self, _workspace: &str, _task: &str) -> std::io::Result<tilo_socket::Stream> {
        let (api, kept) = tilo_socket::pair()?;
        self.held.lock().expect("the held ends").push(kept);
        Ok(api)
    }
}

/// A listing still answers when the task's program it starts never does: the program's answer
/// budget passes, and the listing is read without the names a program gives, not held for ever.
#[test]
fn a_listing_answers_when_the_tasks_program_never_does() {
    let host = Notes::new(true);
    let server = agent(&host)
        .with_agent_session("task:1")
        .with_task_hosts(Some(Arc::new(SilentProgram {
            held: Mutex::new(Vec::new()),
        })))
        .with_task_answer_wait(std::time::Duration::from_millis(100));
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let began = std::time::Instant::now();
            let (listed, failed) = tool(&server, "tilo_ls", json!({"path": "notes"})).await;
            let waited = began.elapsed();
            assert!(!failed, "a listing whose program never answers: {listed}");
            assert!(listed.contains("Todo"), "{listed}");
            assert!(
                waited < std::time::Duration::from_secs(5),
                "the listing waited {waited:?} on a program that never answered"
            );
        });
}
