//! The News Club: a bot in a hosted room's Chat whose personas talk about
//! the stories on the Hacker News front page, with each other and with
//! whoever joins in, so a visitor finds a room that is already talking.
//!
//! The club is one installed bot. Its program does one thing, posting in
//! the room's chat under a persona's name, so the room's governance records
//! exactly that grant and lets it show other names (`Personas`); every
//! member draws its posts with the bot's `BOT` chip whatever name they
//! show. What to post, and when, is decided here on the host: the front
//! page is read, a persona's post is written by the model, and the pace
//! ([`pace`]) spaces the posts the way people's are, sometimes not at all.
//! Its posts are signed by the bot's own device and published by the
//! governor like any installed bot's.

pub mod hn;
pub mod model;
pub mod pace;
pub mod personas;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use serde_json::{Map, Value};
use tilo_agent_participant::{AppCatalogue, Replica, ReplicaActor, Seed};
use tilo_automation::{Binding, Selector, Shortcut, Step, StepKind};
use tilo_user_identity::DeviceCert;

use self::hn::{Stories, Story};
use self::model::{Ask, Line, Writer};
use self::pace::{Club, Dice, Next, Pace, Seen};
use super::installed::InstalledBot;
use crate::rooms::Rooms;

const CHAT: &str = "so.tilo.chat";
/// The directory entry the club is installed as.
pub const ENTRY: &str = "news-club";
/// The name the room shows for the bot.
pub const NAME: &str = "News Club";
/// How often the club looks at the room with nothing waking it.
const WAKE: Duration = Duration::from_secs(60);
/// After a failed look at the front page, how long until the next.
const RETRY_STORY: i64 = 10 * 60;
/// The most of an article read.
const ARTICLE_BYTES: usize = 2 * 1024 * 1024;

/// The club's program: post `text` in `channel`, as a reply to `parent`
/// when there is one, under the name `appearance` gives. Its grant is the
/// one its step needs, as a room admits a bot.
#[must_use]
pub fn program(channel: &str) -> Shortcut {
    use tilo_actions::params::{ActionParam, ParamType};
    let input = |name: &str| Binding::Input(name.to_owned());
    let shortcut = Shortcut {
        id: ENTRY.into(),
        name: NAME.into(),
        inputs: vec![
            ActionParam::required("text", ParamType::Text, "What the persona says"),
            ActionParam::optional("parent", ParamType::Text, "The thread it answers in"),
            ActionParam::required(
                "appearance",
                ParamType::Object,
                "The persona's name, shown beside the bot's chip",
            ),
        ],
        steps: vec![Step::new(
            "post",
            StepKind::Invoke {
                action: tilo_actions::ActionKey::new(CHAT, "chat.messages", "post"),
                resource: Selector::FirstOfKind("chat.messages".into()),
                args: [
                    ("channel".to_owned(), Binding::Literal(channel.into())),
                    ("text".to_owned(), input("text")),
                    ("parent".to_owned(), input("parent")),
                ]
                .into_iter()
                .collect(),
                appearance: Some(input("appearance")),
            },
        )],
        output: None,
        grant: Vec::new(),
        triggers: Vec::new(),
        saved_by: None,
    };
    Shortcut {
        grant: tilo_automation::grant::derived_rules(
            &shortcut,
            &tilo_automation::ActionCatalog::new(Vec::new()),
        ),
        ..shortcut
    }
}

/// The club installed under `dir` posting in `channel`: the installation
/// kept there before, carrying this build's program, else a new one.
///
/// # Errors
/// The program cannot be written or the installation cannot be kept.
pub fn install(dir: &Path, channel: &str) -> Result<InstalledBot, String> {
    let kept = dir.join("bot.json");
    let program = String::from_utf8(
        tilo_automation::program::canonical_bytes(&program(channel))
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let earlier = std::fs::read(&kept)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<InstalledBot>(&bytes).ok())
        .filter(|bot| bot.entry == ENTRY);
    let bot = match earlier {
        Some(bot) if bot.program == program => return Ok(bot),
        // A new build's program: the room's row is updated to it.
        Some(bot) => InstalledBot { program, ..bot },
        None => InstalledBot {
            installation: uuid::Uuid::new_v4().to_string(),
            entry: ENTRY.into(),
            name: NAME.into(),
            program,
            inputs: Map::new(),
            installed_at_ms: tilo_clock::unix_now_ms_i64(),
        },
    };
    std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(&bot).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write(&kept, &bytes).map_err(|error| error.to_string())?;
    Ok(bot)
}

/// The id of the room's channel named `name`, if the chat has it.
fn channel_named(chat: &[u8], name: &str) -> Option<String> {
    let doc = AutoCommit::load(chat).ok()?;
    chat::projection::read(&doc)
        .channel_rows()
        .into_iter()
        .find(|channel| channel.name == name)
        .map(|channel| channel.id)
}

/// Where the club runs.
pub struct Placement {
    /// The rooms this host governs.
    pub rooms: Rooms,
    /// The room the club talks in.
    pub room: String,
    /// The governor's device, a device of the room's root, which installed
    /// the club.
    pub host: DeviceCert,
    /// The club's own device, which signs its posts.
    pub bot_key: ed25519_dalek::SigningKey,
    /// The club's installation.
    pub bot: InstalledBot,
    /// The name of the channel it talks in.
    pub channel: String,
    /// Where it keeps what it follows.
    pub state_file: PathBuf,
}

/// The club's runner.
pub struct NewsClub<W, S> {
    at: Placement,
    writer: Arc<W>,
    front: S,
    pace: Pace,
    club: Club,
    /// Articles read, by story, for as long as the host runs.
    articles: HashMap<u64, Option<String>>,
    web: tilo_net::Client,
}

/// A message of the club's channel, as the club reads it.
#[derive(Clone, Debug)]
struct Said {
    id: String,
    /// The name it shows: a persona's, or the person's.
    name: String,
    /// The persona, when the club wrote it.
    persona: Option<String>,
    parent: Option<String>,
    ts: i64,
    text: String,
}

impl<W: Writer + 'static, S: Stories> NewsClub<W, S> {
    /// The club at `at`, writing with `writer` and taking its stories from
    /// `front`.
    ///
    /// # Errors
    /// The state file is unreadable.
    pub fn new(at: Placement, writer: W, front: S, pace: Pace) -> Result<Self, String> {
        let club = match std::fs::read(&at.state_file) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("{}: {error}", at.state_file.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Club::default(),
            Err(error) => return Err(format!("{}: {error}", at.state_file.display())),
        };
        Ok(Self {
            at,
            writer: Arc::new(writer),
            front,
            pace,
            club,
            articles: HashMap::new(),
            web: tilo_net::Client::new(),
        })
    }

    /// What the club keeps between wakes.
    #[must_use]
    pub const fn club(&self) -> &Club {
        &self.club
    }

    fn actor(&self) -> String {
        format!("bot:{}", self.at.bot.installation)
    }

    /// Run until the room is no longer governed here: a look every minute,
    /// and whenever the chat changes.
    pub async fn run(mut self) {
        let mut writes = self.at.rooms.record_writes();
        loop {
            let now = tilo_clock::unix_now_ms_i64() / 1000;
            if let Err(error) = self.wake(now, &mut pace::Random).await {
                tracing::warn!(room = %self.at.room, %error, "news club did not finish a look");
            }
            let sleep = tokio::time::sleep(WAKE);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    () = &mut sleep => break,
                    write = writes.recv() => match write {
                        Ok(write) if write.room == self.at.room && write.app == CHAT => break,
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    },
                }
            }
        }
    }

    /// Take in the club's threads, then post what is due at `now`
    /// (seconds since the epoch), if anything.
    ///
    /// # Errors
    /// The room's chat could not be read or written, or the state not kept.
    pub async fn wake(&mut self, now: i64, dice: &mut (dyn Dice + Send)) -> Result<(), String> {
        let Some(chat) = self.chat().await? else {
            return Ok(());
        };
        let Some(channel) = channel_named(&chat, &self.at.channel) else {
            tracing::debug!(channel = %self.at.channel, "news club waits for its channel");
            return Ok(());
        };
        let said = self.said(&chat, &channel).await?;
        self.club.retire(now, &self.pace);
        for root in self
            .club
            .threads
            .iter()
            .map(|t| t.root.clone())
            .collect::<Vec<_>>()
        {
            let lines: Vec<Seen> = replies(&said, &root)
                .into_iter()
                .map(|said| Seen {
                    id: said.id.clone(),
                    persona: said.persona.clone(),
                    text: said.text.clone(),
                })
                .collect();
            self.club.take_in(&root, &lines, now, &self.pace, dice);
        }
        match self.club.next(now, &self.pace, dice) {
            Next::Reply(plan) => self.reply(&plan, &said, &channel, now).await?,
            Next::Open { persona } => self.open(&persona, &channel, now, dice).await?,
            Next::Wait => {}
        }
        self.keep()
    }

    /// Post `plan`'s reply, if its persona has something to say.
    async fn reply(
        &mut self,
        plan: &pace::Planned,
        said: &[Said],
        channel: &str,
        now: i64,
    ) -> Result<(), String> {
        let Some(thread) = self
            .club
            .threads
            .iter()
            .find(|t| t.root == plan.root)
            .cloned()
        else {
            return Ok(());
        };
        let Some(persona) = personas::named(&plan.persona).copied() else {
            return Ok(());
        };
        let line = |said: &Said| Line {
            name: said.name.clone(),
            club: said.persona.is_some(),
            text: said.text.clone(),
        };
        let mut lines: Vec<Line> = said
            .iter()
            .filter(|said| said.id == thread.root)
            .map(line)
            .collect();
        lines.extend(replies(said, &thread.root).into_iter().map(line));
        let answering = plan
            .answering
            .as_ref()
            .and_then(|id| said.iter().find(|said| &said.id == id))
            .map(line);
        let article = self.article(&thread.story).await;
        let ask = Ask {
            persona,
            story: thread.story.clone(),
            article,
            thread: lines,
            answering,
        };
        let written = match self.writer.write(&ask).await {
            Ok(written) => written,
            Err(error) => {
                tracing::warn!(%error, "news club's model wrote no reply");
                return Ok(());
            }
        };
        let Some(text) = written.text else {
            return Ok(());
        };
        self.post(persona.name, &text, Some(&thread.root), channel)
            .await?;
        self.club.replied(&thread.root, persona.name, now);
        Ok(())
    }

    /// Open a thread on the next story of the front page, as `persona`.
    async fn open(
        &mut self,
        persona: &str,
        channel: &str,
        now: i64,
        dice: &mut (dyn Dice + Send),
    ) -> Result<(), String> {
        let Some(persona) = personas::named(persona).copied() else {
            return Ok(());
        };
        let club = &self.club;
        let (story, passed) = match self.front.next(&|id| club.took(id)).await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(%error, "news club could not read the front page");
                self.club.next_story = now + RETRY_STORY;
                return Ok(());
            }
        };
        for id in passed {
            self.club.take(id);
        }
        let Some(story) = story else {
            self.club.story_later(now, &self.pace, dice);
            return Ok(());
        };
        let article = self.article(&story).await;
        let ask = Ask {
            persona,
            story: story.clone(),
            article,
            thread: Vec::new(),
            answering: None,
        };
        let written = match self.writer.write(&ask).await {
            Ok(written) => written,
            Err(error) => {
                tracing::warn!(%error, "news club's model wrote no opening");
                self.club.next_story = now + RETRY_STORY;
                return Ok(());
            }
        };
        // A story the model finds political, or has nothing to say about,
        // is left; the next wake looks at the next one.
        let Some(text) = written.text else {
            self.club.take(story.id);
            return Ok(());
        };
        let text = format!("{text}\n\n{}", story.link());
        let root = self.post(persona.name, &text, None, channel).await?;
        self.club.opened(
            (root, story, persona.name.to_owned()),
            now,
            &self.pace,
            dice,
        );
        Ok(())
    }

    /// The readable text of `story`'s article, read once.
    async fn article(&mut self, story: &Story) -> Option<String> {
        if let Some(article) = self.articles.get(&story.id) {
            return article.clone();
        }
        let article = match &story.url {
            Some(url) => {
                let limits = tilo_web_search::FetchLimits {
                    max_bytes: ARTICLE_BYTES,
                    ..tilo_web_search::FetchLimits::default()
                };
                match tilo_web_search::fetch(&self.web, url, &limits).await {
                    Ok(page) => Some(page.text).filter(|text| !text.trim().is_empty()),
                    Err(error) => {
                        tracing::debug!(story = story.id, %error, "news club could not read an article");
                        None
                    }
                }
            }
            None => None,
        };
        self.articles.insert(story.id, article.clone());
        article
    }

    async fn chat(&self) -> Result<Option<Vec<u8>>, String> {
        self.at
            .rooms
            .record_document(&self.at.room, CHAT, "")
            .await
            .map_err(|error| error.to_string())
    }

    /// The parts of the chat's messages the room holds, each whole.
    async fn parts(&self) -> Result<Vec<(ui::record::RecordId, Vec<u8>)>, String> {
        let mut parts = Vec::new();
        for (app, record) in self
            .at
            .rooms
            .record_routes(&self.at.room)
            .await
            .map_err(|error| error.to_string())?
        {
            let Some(part) = ui::record::RecordId::parse(&record)
                .filter(|record| app == CHAT && chat::parts::part_index(record).is_some())
            else {
                continue;
            };
            if let Some(bytes) = self
                .at
                .rooms
                .record_document(&self.at.room, CHAT, &record)
                .await
                .map_err(|error| error.to_string())?
            {
                parts.push((part, bytes));
            }
        }
        Ok(parts)
    }

    /// The messages of `channel`, oldest first, each with the name it
    /// shows.
    async fn said(&self, chat: &[u8], channel: &str) -> Result<Vec<Said>, String> {
        let doc = AutoCommit::load(chat).map_err(|error| error.to_string())?;
        let names: HashMap<String, String> = chat::projection::read(&doc)
            .user_rows()
            .into_iter()
            .map(|user| (user.id, user.name))
            .collect();
        let actor = self.actor();
        let mut said = Vec::new();
        for (_, bytes) in self.parts().await? {
            let part = chat::messages::read_part(
                &AutoCommit::load(&bytes).map_err(|error| error.to_string())?,
            );
            for (id, message) in part.messages.iter() {
                if message.channel.as_str() != channel {
                    continue;
                }
                let shown = message.appearance.as_ref().map(|shown| shown.name.clone());
                let persona = (message.author.0 == actor).then(|| shown.clone()).flatten();
                let name = shown
                    .or_else(|| names.get(&message.author.0).cloned())
                    .unwrap_or_else(|| "Someone".to_owned());
                said.push(Said {
                    id: id.to_string(),
                    name,
                    persona,
                    parent: message.parent.as_ref().map(ToString::to_string),
                    ts: message.ts,
                    text: message.text.clone(),
                });
            }
        }
        said.sort_by_key(|said| said.ts);
        Ok(said)
    }

    /// Post `text` as `persona` in `channel`, in thread `parent` if given,
    /// signed by the club's device. The new message's id.
    async fn post(
        &self,
        persona: &str,
        text: &str,
        parent: Option<&str>,
        channel: &str,
    ) -> Result<String, String> {
        let chat = self.chat().await?.ok_or("the room has no chat")?;
        let parts = self.parts().await?;
        let installer = hex::encode(self.at.host.user_public_key);
        let shortcut = self.at.bot.shortcut()?;
        let standing = super::installed::bot_standing(
            &self.at.room,
            &installer,
            &self.at.bot.installation,
            &shortcut,
            &std::collections::BTreeSet::new(),
        );
        let (room, installation, actor) = (
            self.at.room.clone(),
            self.at.bot.installation.clone(),
            self.actor(),
        );
        let mut arguments = Map::new();
        arguments.insert("channel".into(), Value::from(channel));
        arguments.insert("text".into(), Value::from(text));
        if let Some(parent) = parent {
            arguments.insert("parent".into(), Value::from(parent));
        }
        let appearance = tilo_actions::Appearance {
            name: persona.to_owned(),
            avatar: None,
            persona: None,
        };
        // The replica runs the chat's code in a confined worker, driven
        // synchronously from one thread.
        let (id, changes) = tokio::task::spawn_blocking(move || {
            let mut replica = Replica::open(
                &AppCatalogue::native(),
                CHAT,
                &room,
                &actor,
                ReplicaActor::Bot {
                    installation,
                    installer,
                    standing,
                },
                Seed::Existing {
                    record: ui::record::RecordId::package_primary(),
                    changes: vec![chat],
                },
            )
            .map_err(|error| error.to_string())?;
            for (part, bytes) in parts {
                replica
                    .apply_record(&part, &DocumentMutation::Snapshot(bytes))
                    .map_err(|error| error.to_string())?;
            }
            let before = message_ids(&replica);
            replica
                .invoke_appearing(
                    &tilo_actions::door::ActionAddress::new(
                        ui::record::chain_resource_id(&chat::parts::chain()),
                        "post",
                    ),
                    &arguments,
                    false,
                    Some(appearance),
                )
                .map_err(|error| error.to_string())?;
            let id = message_ids(&replica)
                .into_iter()
                .find(|id| !before.contains(id))
                .ok_or("the post is in no part")?;
            let mut changes = Vec::new();
            if let Some(mutation) = replica.take_mutation() {
                changes.push((String::new(), mutation));
            }
            for (record, mutation) in replica.take_record_mutations() {
                changes.push((record.as_str().to_owned(), mutation));
            }
            Ok::<_, String>((id, changes))
        })
        .await
        .map_err(|error| error.to_string())??;
        for (record, mutation) in changes {
            let bytes = match mutation {
                DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
            };
            self.at
                .rooms
                .publish_as_bot(
                    &self.at.room,
                    &self.at.host,
                    &self.at.bot_key,
                    &AppCatalogue::native(),
                    CHAT,
                    &record,
                    &bytes,
                )
                .await
                .map_err(|error| format!("the room refused the club's post: {error}"))?;
        }
        Ok(id)
    }

    fn keep(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&self.club).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(&self.at.state_file, &bytes)
            .map_err(|error| error.to_string())
    }
}

/// The replies in thread `root`, oldest first.
fn replies<'a>(said: &'a [Said], root: &str) -> Vec<&'a Said> {
    said.iter()
        .filter(|said| said.parent.as_deref() == Some(root))
        .collect()
}

/// The message ids `replica` holds in the parts beside its chat record.
fn message_ids(replica: &Replica) -> Vec<String> {
    replica
        .records_beside()
        .iter()
        .filter_map(|record| replica.read_record(record, chat::messages::read_part))
        .flat_map(|part| {
            part.messages
                .ids()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests;
