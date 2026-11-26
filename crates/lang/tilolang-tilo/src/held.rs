//! An app's held connection: work with no end that the app keeps open to something outside it,
//! such as a language server, a chat gateway, or an SSH or VNC session.
//!
//! A held connection is never an engine job. A job ends; a held connection lives until the app
//! or the person closes it. It runs in the app's own connection process: the app's launch with
//! no window, in the sandbox its package declares, with the network its package was granted
//! (`[sandbox.net]`) and no other. The device's daemon starts that process and holds it, so
//! closing the window or idling out the app's tilolang process ends nothing. A process several
//! apps share, such as the tilolang host that runs the app's instances, never holds one and never
//! sees a secret one uses.
//!
//! A package names the kinds it keeps (`connections: ["lsp"]`). Its code opens one by a key of
//! its own (`connection.open(kind, key, ask)`), sends it messages (`connection.send(key,
//! message)`) and ends it (`connection.close(key)`). What the connection says of itself is a
//! word of JSON the process pushes as it changes. A view reads it (`connection.said(key)`) and
//! runs again when a new word arrives, so nothing polls. An instance that attaches later is
//! handed the newest word of every connection the process holds.
//!
//! Each kind is native code ([`Kind`]), which the connection process runs ([`run`]). A secret a
//! kind needs, such as a gateway's bot token, is named in the open's ask as a vault item. The
//! kind's code releases it into its own process ([`Release`]), never into a record, an argument
//! or a word.
//!
//! The instance's side is [`Client`]: one connection to the process per instance, which the
//! daemon hands over with the instance's other connections.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use serde::{Deserialize, Serialize};

use crate::origin::{read_json_frame, write_json_frame};

/// The largest frame between an instance and its connection process: a whole source file a
/// language server is told, with room to spare.
const FRAME: wire_frame::Framing = wire_frame::Framing::big_endian(64 * 1024 * 1024);

/// A secret released into this process: it derefs to its text and is wiped where it is dropped.
pub type Released = Box<dyn std::ops::Deref<Target = String> + Send>;

/// How a kind's code releases a vault item into its own process.
pub trait Release: Send + Sync {
    /// The secret the vault item `item` (`vault.item:<id>`) keeps, released to this app.
    ///
    /// # Errors
    /// The app is not granted the item, or this device cannot release it.
    fn release(&self, item: &str) -> Result<Released, String>;
}

/// The release of a process the daemon handed no way to release anything: every release is
/// refused, saying why.
pub struct Releases;

impl Release for Releases {
    fn release(&self, item: &str) -> Result<Released, String> {
        Err(format!(
            "{item} cannot be released here: this app's package is not granted the sign-ins it \
             would be released through"
        ))
    }
}

/// What a connection says of itself: one word of JSON, which replaces the one before.
pub type Say = Arc<dyn Fn(&str) + Send + Sync>;

/// What a kind's connection is opened with.
pub struct Opened {
    /// Hand every reader the connection's newest word.
    pub say: Say,
    /// Release a vault item into this process.
    pub release: Arc<dyn Release>,
}

/// One kind of held connection: its native code.
pub trait Kind: Send + Sync {
    /// Open the connection `key` with the JSON `ask` the app gave.
    ///
    /// # Errors
    /// What the app reads as the open's failure: an ask the kind does not take, or a connection
    /// that cannot be made.
    fn open(&self, key: &str, ask: &str, opened: Opened) -> Result<Box<dyn Connection>, String>;
}

/// One open held connection.
pub trait Connection: Send {
    /// One message from the app, as JSON: answered now, with whatever more it says coming as its
    /// word.
    ///
    /// # Errors
    /// A message the connection does not take, or one it could not pass on.
    fn send(&mut self, message: &str) -> Result<String, String>;

    /// End the connection: what it holds outside is let go.
    fn close(&mut self) {}
}

/// What an instance asks its connection process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ask {
    Open {
        kind: String,
        key: String,
        ask: String,
    },
    Send {
        key: String,
        message: String,
    },
    Close {
        key: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct Asked {
    id: u64,
    ask: Ask,
}

/// What a connection process tells an instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Told {
    /// The answer to the ask `id`.
    Answer {
        id: u64,
        result: Result<String, String>,
    },
    /// Connection `key` says `word` of itself now.
    Said { key: String, word: String },
    /// Connection `key` ended: its word goes with it.
    Closed { key: String },
}

fn locked<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The connections a connection process holds, and the instances attached to it.
struct Held {
    kind: Arc<dyn Kind>,
    name: String,
    release: Arc<dyn Release>,
    open: Mutex<BTreeMap<String, Box<dyn Connection>>>,
    talk: Mutex<Talk>,
}

#[derive(Default)]
struct Talk {
    words: BTreeMap<String, String>,
    readers: BTreeMap<u64, Arc<Mutex<tilo_socket::Stream>>>,
    next: u64,
}

impl Talk {
    /// Tell every reader `told`, forgetting a reader whose connection is gone.
    fn tell(&mut self, told: &Told) {
        self.readers
            .retain(|_, reader| write_json_frame(FRAME, &mut *locked(reader), told).is_ok());
    }
}

impl Held {
    fn say(self: &Arc<Self>, key: &str) -> Say {
        let held: Weak<Self> = Arc::downgrade(self);
        let key = key.to_owned();
        Arc::new(move |word: &str| {
            let Some(held) = held.upgrade() else {
                return;
            };
            let mut talk = locked(&held.talk);
            talk.words.insert(key.clone(), word.to_owned());
            talk.tell(&Told::Said {
                key: key.clone(),
                word: word.to_owned(),
            });
        })
    }

    fn answer(self: &Arc<Self>, ask: Ask) -> Result<String, String> {
        match ask {
            Ask::Open { kind, key, ask } => {
                if kind != self.name {
                    return Err(format!(
                        "this app keeps no `{kind}` connection: its connection process holds \
                         `{}`",
                        self.name
                    ));
                }
                let mut open = locked(&self.open);
                if open.contains_key(&key) {
                    return Ok("{}".to_owned());
                }
                let connection = self.kind.open(
                    &key,
                    &ask,
                    Opened {
                        say: self.say(&key),
                        release: Arc::clone(&self.release),
                    },
                )?;
                open.insert(key, connection);
                drop(open);
                Ok("{}".to_owned())
            }
            Ask::Send { key, message } => locked(&self.open)
                .get_mut(&key)
                .ok_or_else(|| format!("no connection `{key}` is open"))?
                .send(&message),
            Ask::Close { key } => {
                let closed = locked(&self.open).remove(&key);
                let Some(mut closed) = closed else {
                    return Ok("false".to_owned());
                };
                closed.close();
                let mut talk = locked(&self.talk);
                talk.words.remove(&key);
                talk.tell(&Told::Closed { key });
                drop(talk);
                Ok("true".to_owned())
            }
        }
    }

    /// Whether nothing is held and nobody is attached: the process has nothing left to do.
    fn idle(&self) -> bool {
        locked(&self.open).is_empty() && locked(&self.talk).readers.is_empty()
    }

    fn close_all(&self) {
        let open = std::mem::take(&mut *locked(&self.open));
        for (_, mut connection) in open {
            connection.close();
        }
    }

    /// Answer one attached instance until its connection ends.
    fn attach(self: &Arc<Self>, stream: tilo_socket::Stream, idle: &Arc<dyn Fn() + Send + Sync>) {
        let mut reading = match stream.try_clone() {
            Ok(reading) => reading,
            Err(error) => {
                tracing::warn!(%error, "an instance's connection to its held connections");
                return;
            }
        };
        let writer = Arc::new(Mutex::new(stream));
        let id = {
            let mut talk = locked(&self.talk);
            talk.next = talk.next.wrapping_add(1);
            let id = talk.next;
            // An instance that attaches later reads what every connection said last.
            let words: Vec<Told> = talk
                .words
                .iter()
                .map(|(key, word)| Told::Said {
                    key: key.clone(),
                    word: word.clone(),
                })
                .collect();
            let mut out = locked(&writer);
            if words
                .iter()
                .any(|told| write_json_frame(FRAME, &mut *out, told).is_err())
            {
                return;
            }
            drop(out);
            talk.readers.insert(id, Arc::clone(&writer));
            id
        };
        let held = Arc::clone(self);
        let idle = Arc::clone(idle);
        let spawned = std::thread::Builder::new()
            .name("held-connection".into())
            .spawn(move || {
                while let Ok(Some(asked)) = read_json_frame::<Asked>(FRAME, &mut reading) {
                    // A connection this ask opens, rather than joins: undone when the instance
                    // that asked for it is gone before it hears that it is open.
                    let opening = match &asked.ask {
                        Ask::Open { key, .. } if !locked(&held.open).contains_key(key) => {
                            Some(key.clone())
                        }
                        _ => None,
                    };
                    let result = held.answer(asked.ask);
                    let opened = result.is_ok();
                    let told = Told::Answer {
                        id: asked.id,
                        result,
                    };
                    // The talk lock orders the answer among the words told to this reader.
                    let talk = locked(&held.talk);
                    let failed = write_json_frame(FRAME, &mut *locked(&writer), &told).is_err();
                    drop(talk);
                    if failed {
                        if let Some(key) = opening.filter(|_| opened) {
                            let _ = held.answer(Ask::Close { key });
                        }
                        break;
                    }
                }
                locked(&held.talk).readers.remove(&id);
                if held.idle() {
                    idle();
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "an instance's held connections are not answered");
            locked(&self.talk).readers.remove(&id);
        }
    }
}

/// Serve the connections of the kind `name` to each instance `readers` hands over, until it hands
/// no more. `idle` is called when no connection is open and no instance is attached any more.
/// Every connection still open when `readers` ends is closed.
pub fn serve(
    name: &str,
    kind: Arc<dyn Kind>,
    release: Arc<dyn Release>,
    readers: impl Iterator<Item = tilo_socket::Stream>,
    idle: &Arc<dyn Fn() + Send + Sync>,
) {
    let held = Arc::new(Held {
        kind,
        name: name.to_owned(),
        release,
        open: Mutex::default(),
        talk: Mutex::default(),
    });
    for reader in readers {
        held.attach(reader, idle);
    }
    held.close_all();
}

/// What the instance's side does with a word its connection process told it.
pub trait Heard: Send + Sync {
    /// Connection `key` says `word` of itself now; an empty word when it ended.
    fn said(&self, key: &str, word: &str);
}

/// An instance's connection to its app's connection process.
pub struct Client {
    writer: Mutex<tilo_socket::Stream>,
    shared: Arc<ClientShared>,
}

/// Who waits for the answer to one ask.
type Waiter = std::sync::mpsc::Sender<Result<String, String>>;

#[derive(Default)]
struct ClientShared {
    next: Mutex<u64>,
    /// Who waits for an answer, by the ask's id; `None` once the process went away, so no ask
    /// waits for an answer that cannot come.
    waiting: Mutex<Option<BTreeMap<u64, Waiter>>>,
    words: Mutex<BTreeMap<String, String>>,
    heard: Mutex<Option<Arc<dyn Heard>>>,
}

impl Client {
    /// The client over `stream`, the instance's end of its connection to the process.
    ///
    /// # Errors
    /// The stream cannot be read on a thread of its own.
    pub fn over(stream: tilo_socket::Stream) -> io::Result<Self> {
        let mut reading = stream.try_clone()?;
        let shared = Arc::new(ClientShared {
            waiting: Mutex::new(Some(BTreeMap::new())),
            ..ClientShared::default()
        });
        let theirs = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("held-connection-client".into())
            .spawn(move || {
                while let Ok(Some(told)) = read_json_frame::<Told>(FRAME, &mut reading) {
                    theirs.told(told);
                }
                // Whoever still waits is told the process went away, and nobody waits again.
                let waiting = locked(&theirs.waiting).take().unwrap_or_default();
                for (_, waiter) in waiting {
                    let _ = waiter.send(Err(ENDED.to_owned()));
                }
            })?;
        Ok(Self {
            writer: Mutex::new(stream),
            shared,
        })
    }

    /// Hand every word from now on to `heard`, starting with the newest word of every
    /// connection the process said something of already.
    pub fn hear(&self, heard: &Arc<dyn Heard>) {
        let words = locked(&self.shared.words).clone();
        *locked(&self.shared.heard) = Some(Arc::clone(heard));
        for (key, word) in words {
            heard.said(&key, &word);
        }
    }

    /// The newest word connection `key` said; empty while it said nothing.
    #[must_use]
    pub fn said(&self, key: &str) -> String {
        locked(&self.shared.words)
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    /// Ask the process `ask`, waiting for its answer.
    ///
    /// # Errors
    /// The process refused, or it is gone.
    pub fn ask(&self, ask: Ask) -> Result<String, String> {
        let (send, answered) = std::sync::mpsc::channel();
        let id = {
            let mut next = locked(&self.shared.next);
            *next = next.wrapping_add(1);
            *next
        };
        locked(&self.shared.waiting)
            .as_mut()
            .ok_or_else(|| ENDED.to_owned())?
            .insert(id, send);
        let written = write_json_frame(FRAME, &mut *locked(&self.writer), &Asked { id, ask });
        if let Err(error) = written {
            if let Some(waiting) = locked(&self.shared.waiting).as_mut() {
                waiting.remove(&id);
            }
            return Err(format!("{ENDED}: {error}"));
        }
        answered.recv().unwrap_or_else(|_| Err(ENDED.to_owned()))
    }
}

impl Drop for Client {
    /// The instance went: the process is told at once, though the client's reader still holds
    /// its half of the connection.
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = locked(&self.writer).shutdown(std::net::Shutdown::Both);
        }
    }
}

/// What an ask of a connection process that went away answers.
const ENDED: &str = "the app's connection process is gone";

impl ClientShared {
    fn told(&self, told: Told) {
        let (key, word) = match told {
            Told::Answer { id, result } => {
                let waiter = locked(&self.waiting)
                    .as_mut()
                    .and_then(|waiting| waiting.remove(&id));
                if let Some(waiter) = waiter {
                    let _ = waiter.send(result);
                }
                return;
            }
            Told::Said { key, word } => {
                locked(&self.words).insert(key.clone(), word.clone());
                (key, word)
            }
            Told::Closed { key } => {
                locked(&self.words).remove(&key);
                (key, String::new())
            }
        };
        let heard = locked(&self.heard).clone();
        if let Some(heard) = heard {
            heard.said(&key, &word);
        }
    }
}

/// An instance's held connections, as its runtime asks them (`connection.*`): its [`Client`] of
/// the app's connection process, waking the instance with `wake` whenever a connection says
/// something new.
pub struct Connections {
    pub client: Client,
    pub wake: crate::background::Waker,
}

/// Each word heard, kept where the runtime's views read it, and the instance woken for it.
struct Pushed {
    push: Arc<dyn tilolang_engine::Push>,
    wake: crate::background::Waker,
}

impl Heard for Pushed {
    fn said(&self, key: &str, word: &str) {
        self.push
            .state(&tilolang_runtime::host_work::held_slot(key), word);
        (self.wake)();
    }
}

impl tilolang_runtime::host_work::HeldHost for Connections {
    fn open(&self, _app: &str, kind: &str, key: &str, ask: &str) -> Result<String, String> {
        self.client.ask(Ask::Open {
            kind: kind.to_owned(),
            key: key.to_owned(),
            ask: ask.to_owned(),
        })
    }

    fn send(&self, key: &str, message: &str) -> Result<String, String> {
        self.client.ask(Ask::Send {
            key: key.to_owned(),
            message: message.to_owned(),
        })
    }

    fn close(&self, key: &str) -> bool {
        self.client
            .ask(Ask::Close {
                key: key.to_owned(),
            })
            .is_ok_and(|closed| closed == "true")
    }

    fn follow(&self, push: Arc<dyn tilolang_engine::Push>) {
        let pushed: Arc<dyn Heard> = Arc::new(Pushed {
            push,
            wake: Arc::clone(&self.wake),
        });
        self.client.hear(&pushed);
    }
}

#[cfg(all(test, unix))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    const SECRET: &str = "bot-token-7f3a";

    /// A kind that releases the vault item its ask names, says what it is sent as its word, and
    /// uses the secret without ever saying it (its length goes into the word).
    struct Echo {
        closed: mpsc::Sender<String>,
        /// An ask of `gated` says it began here, then opens once the test lets it.
        began: Mutex<mpsc::Sender<()>>,
        gate: Mutex<mpsc::Receiver<()>>,
    }

    struct Echoing {
        key: String,
        say: Say,
        secret: Released,
        closed: mpsc::Sender<String>,
    }

    impl Kind for Echo {
        fn open(
            &self,
            key: &str,
            ask: &str,
            opened: Opened,
        ) -> Result<Box<dyn Connection>, String> {
            if ask == "refuse" {
                return Err("this connection cannot be made".to_owned());
            }
            let item = if ask == "gated" {
                let _ = locked(&self.began).send(());
                let _ = locked(&self.gate).recv();
                "vault.item:granted"
            } else {
                ask
            };
            let secret = opened.release.release(item)?;
            (opened.say)("{\"state\":\"ready\"}");
            Ok(Box::new(Echoing {
                key: key.to_owned(),
                say: opened.say,
                secret,
                closed: self.closed.clone(),
            }))
        }
    }

    impl Connection for Echoing {
        fn send(&mut self, message: &str) -> Result<String, String> {
            (self.say)(&format!(
                "{{\"heard\":{message:?},\"signed\":{}}}",
                self.secret.len()
            ));
            Ok("{\"took\":true}".to_owned())
        }

        fn close(&mut self) {
            let _ = self.closed.send(self.key.clone());
        }
    }

    /// The vault, as the daemon releases into the app's own process: only the item the app is
    /// granted.
    struct Vault;

    impl Release for Vault {
        fn release(&self, item: &str) -> Result<Released, String> {
            if item == "vault.item:granted" {
                Ok(Box::new(Box::new(SECRET.to_owned())))
            } else {
                Err(format!("{item} is not released to this app"))
            }
        }
    }

    /// What an instance's runtime keeps of what it heard.
    #[derive(Default)]
    struct Kept {
        words: Mutex<Vec<(String, String)>>,
        told: Mutex<Option<mpsc::Sender<()>>>,
    }

    impl Heard for Kept {
        fn said(&self, key: &str, word: &str) {
            locked(&self.words).push((key.to_owned(), word.to_owned()));
            if let Some(told) = locked(&self.told).as_ref() {
                let _ = told.send(());
            }
        }
    }

    /// The app's connection process, serving the instances handed to it.
    struct Process {
        attach: mpsc::Sender<tilo_socket::Stream>,
        idle: mpsc::Receiver<()>,
        closed: mpsc::Receiver<String>,
        began: mpsc::Receiver<()>,
        gate: mpsc::Sender<()>,
    }

    fn process() -> Process {
        let (attach, readers) = mpsc::channel::<tilo_socket::Stream>();
        let (idled, idle) = mpsc::channel();
        let (closing, closed) = mpsc::channel();
        let (beginning, began) = mpsc::channel();
        let (gate, gated) = mpsc::channel();
        std::thread::spawn(move || {
            let idled = Mutex::new(idled);
            serve(
                "echo",
                Arc::new(Echo {
                    closed: closing,
                    began: Mutex::new(beginning),
                    gate: Mutex::new(gated),
                }),
                Arc::new(Vault),
                readers.into_iter(),
                &(Arc::new(move || {
                    let _ = locked(&idled).send(());
                }) as Arc<dyn Fn() + Send + Sync>),
            );
        });
        Process {
            attach,
            idle,
            closed,
            began,
            gate,
        }
    }

    impl Process {
        /// An instance attached, as the daemon hands one over with a window.
        fn instance(&self) -> (Client, Arc<Kept>, mpsc::Receiver<()>) {
            let (ours, theirs) = tilo_socket::pair().unwrap();
            self.attach.send(theirs).unwrap();
            let client = Client::over(ours).unwrap();
            let kept = Arc::new(Kept::default());
            let (told, heard) = mpsc::channel();
            *locked(&kept.told) = Some(told);
            client.hear(&(Arc::clone(&kept) as Arc<dyn Heard>));
            (client, kept, heard)
        }
    }

    fn open(client: &Client, key: &str, ask: &str) -> Result<String, String> {
        client.ask(Ask::Open {
            kind: "echo".to_owned(),
            key: key.to_owned(),
            ask: ask.to_owned(),
        })
    }

    fn send(client: &Client, key: &str, message: &str) -> Result<String, String> {
        client.ask(Ask::Send {
            key: key.to_owned(),
            message: message.to_owned(),
        })
    }

    fn wait(heard: &mpsc::Receiver<()>) {
        heard
            .recv_timeout(Duration::from_secs(5))
            .expect("a word arrives");
    }

    #[test]
    fn a_held_connection_opens_and_says_what_it_heard() {
        let process = process();
        let (client, kept, heard) = process.instance();
        assert_eq!(
            open(&client, "one", "vault.item:granted"),
            Ok("{}".to_owned())
        );
        wait(&heard);
        assert_eq!(client.said("one"), "{\"state\":\"ready\"}");
        assert_eq!(
            send(&client, "one", "hello"),
            Ok("{\"took\":true}".to_owned())
        );
        wait(&heard);
        // The secret was used in the app's own process and never reached the instance.
        assert_eq!(
            client.said("one"),
            format!("{{\"heard\":\"hello\",\"signed\":{}}}", SECRET.len())
        );
        assert!(
            locked(&kept.words)
                .iter()
                .all(|(_, word)| !word.contains(SECRET)),
            "no word carries the secret"
        );
        // Opening it again joins the one open.
        assert_eq!(
            open(&client, "one", "vault.item:granted"),
            Ok("{}".to_owned())
        );
    }

    #[test]
    fn what_is_not_granted_or_not_open_is_refused() {
        let process = process();
        let (client, _, _) = process.instance();
        let refused = open(&client, "one", "vault.item:someone-elses").unwrap_err();
        assert!(refused.contains("not released to this app"), "{refused}");
        assert!(open(&client, "two", "refuse").is_err());
        let unopened = send(&client, "one", "hello").unwrap_err();
        assert!(
            unopened.contains("no connection `one` is open"),
            "{unopened}"
        );
        let other = client
            .ask(Ask::Open {
                kind: "ssh".to_owned(),
                key: "one".to_owned(),
                ask: String::new(),
            })
            .unwrap_err();
        assert!(other.contains("keeps no `ssh` connection"), "{other}");
    }

    #[test]
    fn a_connection_outlives_its_window_and_ends_when_closed() {
        let process = process();
        let (first, _, heard) = process.instance();
        open(&first, "one", "vault.item:granted").unwrap();
        wait(&heard);
        send(&first, "one", "before").unwrap();
        wait(&heard);
        // The window closes: its instance goes, and the connection stays held.
        drop(first);
        assert!(
            process
                .idle
                .recv_timeout(Duration::from_millis(300))
                .is_err(),
            "a process that holds a connection is not idle"
        );
        assert!(process.closed.try_recv().is_err(), "nothing was closed");

        // A later instance is handed what the connection said last, and reaches it.
        let (later, kept, heard) = process.instance();
        wait(&heard);
        assert!(
            locked(&kept.words)
                .iter()
                .any(|(key, word)| key == "one" && word.contains("before")),
            "the newest word reaches a later instance"
        );
        send(&later, "one", "after").unwrap();
        wait(&heard);
        assert!(later.said("one").contains("after"));

        // Closing it ends it: its code lets go, its word goes, and the process has nothing left
        // once the instance goes too.
        assert_eq!(
            later.ask(Ask::Close {
                key: "one".to_owned()
            }),
            Ok("true".to_owned())
        );
        assert_eq!(
            process.closed.recv_timeout(Duration::from_secs(5)),
            Ok("one".to_owned())
        );
        wait(&heard);
        assert_eq!(later.said("one"), "");
        assert_eq!(
            locked(&kept.words).last().cloned(),
            Some(("one".to_owned(), String::new()))
        );
        drop(later);
        process
            .idle
            .recv_timeout(Duration::from_secs(5))
            .expect("the process says it is idle");
    }

    #[test]
    fn an_instance_whose_process_went_away_is_told() {
        let (ours, theirs) = tilo_socket::pair().unwrap();
        let client = Client::over(ours).unwrap();
        drop(theirs);
        let refused = open(&client, "one", "").unwrap_err();
        assert!(refused.contains("connection process is gone"), "{refused}");
    }

    /// An instance whose call is cancelled while its connection is still opening (its window
    /// closed, its process ended) leaves nothing open behind it: the call ends at once, refused,
    /// and the connection it would have opened is closed as soon as it opened, so the process has
    /// nothing left to hold.
    #[test]
    fn an_open_cancelled_before_it_answered_is_undone() {
        let process = process();
        let (ours, theirs) = tilo_socket::pair().unwrap();
        process.attach.send(theirs).unwrap();
        let cancel = ours.try_clone().unwrap();
        let client = Arc::new(Client::over(ours).unwrap());
        let asking = Arc::clone(&client);
        let asked = std::thread::spawn(move || open(&asking, "slow", "gated"));
        process
            .began
            .recv_timeout(Duration::from_secs(5))
            .expect("the open began");
        cancel.shutdown(std::net::Shutdown::Both).unwrap();
        let answer = asked.join().unwrap();
        assert!(
            answer.as_ref().is_err_and(|why| why.contains(ENDED)),
            "a cancelled call ends refused: {answer:?}"
        );
        process.gate.send(()).unwrap();
        assert_eq!(
            process.closed.recv_timeout(Duration::from_secs(5)),
            Ok("slow".to_owned()),
            "the connection nobody heard of is closed"
        );
        process
            .idle
            .recv_timeout(Duration::from_secs(5))
            .expect("nothing is held for anyone");
        assert_eq!(client.said("slow"), "");
    }
}
