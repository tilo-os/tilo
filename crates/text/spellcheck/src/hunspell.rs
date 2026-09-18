//! Persistent Hunspell process adapter.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Session {
    fn start(dictionary: &str) -> Option<Self> {
        let mut child = Command::new("hunspell")
            .args(["-a", "-d", dictionary])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let mut session = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        };
        let mut banner = String::new();
        (session.stdout.read_line(&mut banner).ok()? > 0).then_some(session)
    }

    fn query(&mut self, words: &[&str]) -> Option<Vec<String>> {
        for word in words {
            if word.contains(['\n', '\r']) {
                return None;
            }
            writeln!(self.stdin, "{word}").ok()?;
        }
        self.stdin.flush().ok()?;

        let mut responses = Vec::with_capacity(words.len());
        while responses.len() < words.len() {
            let mut line = String::new();
            if self.stdout.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let line = line.trim_end_matches(['\n', '\r']);
            if !line.is_empty() {
                responses.push(line.to_string());
            }
        }
        Some(responses)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn lines(language: &str, words: &[&str]) -> Option<Vec<String>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, Option<Session>>>> = OnceLock::new();
    let dictionary = super::normalize_language(language)?;
    let sessions = SESSIONS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut sessions = sessions.lock().ok()?;
    if !sessions.contains_key(&dictionary) {
        sessions.insert(dictionary.clone(), Session::start(&dictionary));
    }
    let result = sessions.get_mut(&dictionary)?.as_mut()?.query(words);
    if result.is_none() {
        sessions.insert(dictionary, None);
    }
    result
}

pub(super) fn results(language: &str, words: &[&str]) -> Option<Vec<bool>> {
    let results: Vec<bool> = lines(language, words)?
        .iter()
        .map(|line| matches!(line.as_bytes().first(), Some(b'*' | b'+' | b'-')))
        .collect();
    (results.len() == words.len()).then_some(results)
}

pub(super) fn suggestions(language: &str, word: &str) -> Option<Vec<String>> {
    let output = lines(language, &[word])?;
    let result = output.first()?;
    let (_, candidates) = result.split_once(':')?;
    Some(
        candidates
            .split(',')
            .map(str::trim)
            .filter(|candidate| !candidate.is_empty())
            .map(str::to_string)
            .collect(),
    )
}
