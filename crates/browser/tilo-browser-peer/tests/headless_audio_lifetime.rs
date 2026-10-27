#![cfg(target_os = "linux")]

use std::fs;
use std::path::Path;
use std::process::Command;

const OBSERVE_ENDPOINT: &str = r"
import os, pathlib, stat, sys
audio = os.environ['TILO_HEADLESS_AUDIO_PID']
identity = pathlib.Path(f'/proc/{audio}/stat').read_text().split(') ')[1].split()
assert identity[1] == str(os.getppid()), 'endpoint must belong to the wrapper'
assert pathlib.Path(f'/proc/{audio}/comm').read_text().strip() == 'pulseaudio'
runtime = pathlib.Path(os.environ['PULSE_RUNTIME_PATH'])
assert stat.S_IMODE(runtime.stat().st_mode) == 0o700
assert stat.S_ISSOCK((runtime / 'native').stat().st_mode)
assert os.environ['PULSE_SERVER'] == f'unix:{runtime}/native'
print(audio, flush=True)
print(identity[19], flush=True)
print(runtime, flush=True)
sys.exit(int(sys.argv[1]))
";

#[test]
fn completed_audio_commands_reap_their_private_daemon_and_preserve_status() {
    tilo_test_env::require_isolated_desktop();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    for status in [0, 17] {
        let output = Command::new("bash")
            .arg(root.join("tools/with-headless-audio.sh"))
            .args(["python3", "-c", OBSERVE_ENDPOINT, &status.to_string()])
            .output()
            .expect("run the production wrapper with its real PulseAudio daemon");
        assert_eq!(
            output.status.code(),
            Some(status),
            "production audio command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let mut lines = stdout.lines();
        let pid: u32 = lines.next().expect("actual daemon PID").parse().unwrap();
        let started = lines.next().expect("actual daemon start time");
        let runtime = Path::new(lines.next().expect("actual private runtime directory"));
        let daemon = format!("/proc/{pid}/comm");
        let surviving = process_file(&daemon).expect("observe owned daemon identity");
        // Clean up only this invocation's actual child on a failing baseline.
        // No process-name scan can kill another test's audio endpoint.
        let same_process = process_file(format!("/proc/{pid}/stat"))
            .expect("observe owned daemon start time")
            .and_then(|stat| stat.split_once(") ").map(|(_, fields)| fields.to_owned()))
            .and_then(|fields| fields.split_whitespace().nth(19).map(str::to_owned))
            .is_some_and(|actual| actual == started);
        if same_process && surviving.as_deref() == Some("pulseaudio\n") {
            Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .expect("clean the leaked daemon after recording the failure");
        }
        assert!(
            !same_process,
            "private audio daemon {pid} survived command exit"
        );
        assert!(
            !runtime.exists(),
            "private audio runtime survived command exit"
        );
    }
}

fn process_file(path: impl AsRef<Path>) -> std::io::Result<Option<String>> {
    match fs::read_to_string(path.as_ref()) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        result => result.map(Some),
    }
}
