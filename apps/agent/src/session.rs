//! One agent session: join the room, hold the replica, act on instructions.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ui::collaboration::{CollabEvent, CollabRequest, PresencePayload};
use ui::presence::{PathSegment, PresenceKey};
use uuid::Uuid;
use word_processor::actions::{self as wp, RegionKind, WordProcessorAction};

use crate::apply::Editor;
use crate::model::{Chooser, Decision, Route};
use crate::plan::{self, Block, Instruction};
use crate::wire::Link;

#[derive(Clone, Debug)]
pub struct Config {
    /// The broker socket; `None` resolves the session default.
    pub socket: Option<PathBuf>,
    /// The app whose document this is — its manifest package id, which is
    /// what a launched copy of the app announces.
    pub app_id: String,
    /// The room (workspace) the document lives in.
    pub cluster: Uuid,
    /// The record the app announced for the document (`file:<path>`), or
    /// `None` for an untitled document.
    pub record: Option<String>,
    /// The agent's name: the caret pill, the history author, and the word
    /// instructions start with.
    pub name: String,
    /// Pause between edits, so people can watch the caret work.
    pub pace: Duration,
    /// Stop after the first instruction has been carried out.
    pub once: bool,
    /// Let a model choose which exported action answers an instruction.
    /// False pins the run to the keyword vocabulary.
    pub model: bool,
    /// How long that call may take before the keyword vocabulary decides
    /// instead. Short on purpose: the work must not wait on the network.
    pub model_timeout: Duration,
    /// How long to wait for somebody in the room to answer with the
    /// document before giving up and saying which address went
    /// unanswered. `Duration::ZERO` waits forever — right for an agent
    /// left running against a room whose document nobody has opened yet,
    /// wrong for a scripted run, where it turns a mismatched route into
    /// a hang that names no cause.
    pub await_document: Duration,
}

pub struct Session {
    cfg: Config,
    link: Link,
    editor: Option<Editor>,
    /// Instruction blocks already acted on, so a slow peer re-sending the
    /// same state does not make the agent do the work twice.
    handled: HashSet<String>,
    /// Decides which exported action answers an instruction.
    chooser: Chooser,
    /// When the agent last asked the room for the document. It asks again
    /// until it has one: an app that was still opening its file when the
    /// agent arrived never saw the first ask, and the agent would then wait
    /// for a reply that is never coming. On a loaded machine that is the
    /// difference between the beat happening and the beat hanging.
    asked_at: Instant,
    /// How the last instruction was decided. The record that keeps a run on
    /// the fallback from being indistinguishable from a live one afterwards.
    last_route: Option<Route>,
}

/// How often the agent asks again while it still has no document.
const REASK_EVERY: Duration = Duration::from_millis(750);

/// Ask the room for the document: peers already in it answer an `Arrived`
/// with their full state. Safe to repeat — a second copy of a document the
/// agent already holds merges to the same thing.
fn ask_for_document(link: &mut Link) -> Result<()> {
    link.send(&CollabRequest::Presence {
        key: PresenceKey::Workspace {
            id: "word-processor".to_string(),
        },
        payload: PresencePayload::Arrived,
    })
}

/// How the word processor keys a caret: the block's own presence key.
fn block_key(block_id: &str) -> PresenceKey {
    PresenceKey::Widget {
        path: vec![
            PathSegment::Field("wp_block".to_string()),
            PathSegment::Field(block_id.to_string()),
        ],
    }
}

impl Session {
    /// Connect, announce the route, and ask the room for the document.
    pub fn connect(cfg: Config) -> Result<Self> {
        let path = cfg
            .socket
            .clone()
            .unwrap_or_else(crate::wire::default_socket_path);
        let mut link = Link::connect(&path)?;
        // The name here is self-declared, and a self-declared name is
        // spoofable — which is why a *peer's* name is resolved through the
        // room document, under the authority that admitted it, rather than
        // taken from what the peer says it is called. The agent should end
        // up resolving the same way. What keeps this honest meanwhile is
        // that the agent is admitted by a capability its inviter granted,
        // so nothing false is being asserted about who is in the room; the
        // name is a label on a participant that was already let in.
        link.send(&CollabRequest::Hello {
            app_id: cfg.app_id.clone(),
            cluster_id: cfg.cluster,
            record: cfg.record.clone(),
            handle: Some(cfg.name.clone()),
        })?;
        // The late-join handshake: every peer in the room answers an
        // `Arrived` with its full document.
        ask_for_document(&mut link)?;
        tracing::info!(
            socket = %path.display(),
            app = %cfg.app_id,
            room = %cfg.cluster,
            record = ?cfg.record,
            name = %cfg.name,
            "joined the room; waiting for the document"
        );
        let chooser = Chooser::new(cfg.model, cfg.model_timeout);
        // Said once, at the top of the run: an operator should know which
        // path a recording is going to take before the beat, not after.
        tracing::info!(
            live = chooser.is_live(),
            model = crate::model::MODEL,
            timeout_ms = cfg.model_timeout.as_millis() as u64,
            "action choice: {}",
            if chooser.is_live() {
                "a model chooses from the app's exported actions"
            } else {
                "no model available; the keyword vocabulary chooses"
            }
        );
        Ok(Self {
            cfg,
            link,
            editor: None,
            handled: HashSet::new(),
            chooser,
            last_route: None,
            asked_at: Instant::now(),
        })
    }

    /// How the last instruction was decided, for a test or an operator
    /// checking after the fact whether a run was live.
    pub fn last_route(&self) -> Option<&Route> {
        self.last_route.as_ref()
    }

    /// Run until the broker goes away — or, with `once`, until the first
    /// instruction has been carried out.
    ///
    /// If nobody ever answers with a document, give up after
    /// `await_document` and say which address went unanswered. Waiting
    /// forever was the previous behaviour and it is the wrong one: the
    /// commonest cause is a route that does not match, the broker drops
    /// a mismatched frame silently by design, and the symptom is an
    /// agent sitting at 0% forever while every log line says it joined
    /// fine. A harness then reports "the agent did not finish", which
    /// names the one component that was working.
    pub fn run(&mut self) -> Result<()> {
        let joined = Instant::now();
        loop {
            if self.step(Duration::from_millis(100))? && self.cfg.once {
                return Ok(());
            }
            if self.editor.is_none()
                && !self.cfg.await_document.is_zero()
                && joined.elapsed() >= self.cfg.await_document
            {
                anyhow::bail!("{}", self.unanswered());
            }
        }
    }

    /// Why no document arrived, in terms of the thing that decides it.
    ///
    /// The broker's fan-out matches `(app, room, record)` and the record
    /// is the innermost test — it fails closed both ways, and "no
    /// record" is a value like any other rather than a wildcard. So an
    /// agent told nothing about the record does not join an untitled
    /// conversation *and* a saved one: it joins the untitled one only,
    /// and a word processor with a file open is announcing
    /// `file:<path>` and cannot hear it.
    fn unanswered(&self) -> String {
        let waited = self.cfg.await_document.as_secs();
        let addressed = match self.cfg.record.as_deref() {
            Some(r) => format!("record {r:?}"),
            None => "no record (an untitled document)".to_string(),
        };
        format!(
            "no participant answered with a document in {waited}s. Announced \
             app={:?} room={} and {addressed}. The broker matches all three \
             and drops what does not match, so if the app has a file open \
             its record is `file:<path>` and this agent must be given the \
             same one with --record.",
            self.cfg.app_id, self.cfg.cluster,
        )
    }

    /// Run until `deadline`, then return whether an instruction was
    /// carried out. For tests.
    pub fn run_until(&mut self, deadline: Instant) -> Result<bool> {
        let mut acted = false;
        while Instant::now() < deadline {
            acted |= self.step(Duration::from_millis(50))?;
            if acted && self.cfg.once {
                break;
            }
        }
        Ok(acted)
    }

    /// One turn: absorb whatever the room sent, then act if the document
    /// asks. Returns whether an instruction was carried out.
    fn step(&mut self, wait: Duration) -> Result<bool> {
        let mut changed = false;
        // Still empty-handed: ask again. An app that was still opening its
        // file when the agent arrived never saw the first ask, and nothing
        // else would ever prompt a reply.
        if self.editor.is_none() && self.asked_at.elapsed() >= REASK_EVERY {
            ask_for_document(&mut self.link)?;
            self.asked_at = Instant::now();
            tracing::debug!("asked the room for the document again");
        }
        match self.link.recv(wait)? {
            Some(CollabEvent::CRDTUpdate { data }) => {
                changed = self.absorb_snapshot(&data)?;
            }
            Some(CollabEvent::CRDTIncremental { data }) => {
                if let Some(ed) = self.editor.as_mut() {
                    changed = ed.merge_incremental(&data)?;
                }
            }
            Some(CollabEvent::RemotePresenceUpdate {
                user_id,
                payload: PresencePayload::Arrived,
                ..
            }) => {
                // A participant arriving after us deserves what we hold.
                if let Some(ed) = self.editor.as_mut() {
                    let full = ed.full();
                    self.link.send(&CollabRequest::SyncCRDT { data: full })?;
                    tracing::debug!(%user_id, "sent our document to a new participant");
                }
            }
            Some(_) | None => {}
        }
        if !changed {
            return Ok(false);
        }
        self.act()
    }

    /// Take a peer's whole-document frame, if it is ours to take.
    ///
    /// A room is not one document. Every participant answers an arrival
    /// with its own state, and the broker hands a frame to any
    /// subscriber whose address it matches — which, for a participant
    /// that announced no app id, is everyone in the room. So the agent
    /// is offered the file manager's state alongside the page it was
    /// invited to, and nothing in the frame says which is which.
    ///
    /// Given a `--record` the address settles it and only one arrives.
    /// Without one, this does what it can from the content: take the
    /// first thing that is a document at all, and if a later one turns
    /// out to be a *different* document — established by its refusing to
    /// merge — prefer whichever of the two is actually asking us for
    /// something.
    ///
    /// Returns whether the agent's replica moved.
    fn absorb_snapshot(&mut self, data: &[u8]) -> Result<bool> {
        let Some(ed) = self.editor.as_mut() else {
            if !crate::apply::looks_like_a_document(data) {
                tracing::debug!(
                    bytes = data.len(),
                    "ignored a peer's state: it is not a document"
                );
                return Ok(false);
            }
            self.editor = Some(Editor::from_snapshot(data, &self.cfg.name)?);
            tracing::info!(bytes = data.len(), "received the document");
            return Ok(true);
        };
        if ed.merge_full(data)? {
            return Ok(true);
        }
        // It did not merge, so it is not this document. Worth having only
        // if it is the one addressed to us and what we hold is not.
        let ours_asks = ed
            .snapshot()
            .map(|s| plan::find_addressed(&plan::blocks(&s), &self.cfg.name).is_some())
            .unwrap_or(false);
        if !ours_asks && Editor::is_addressed_to(data, &self.cfg.name) {
            ed.replace_with(data)?;
            tracing::info!(
                bytes = data.len(),
                "took a different document: this is the one addressed to {}",
                self.cfg.name
            );
            return Ok(true);
        }
        tracing::debug!(
            bytes = data.len(),
            "ignored another document in this room"
        );
        Ok(false)
    }

    /// Look for an instruction and carry it out.
    fn act(&mut self) -> Result<bool> {
        let Some(ed) = self.editor.as_ref() else {
            return Ok(false);
        };
        let blocks = plan::blocks(&ed.snapshot()?);
        // Who was addressed is decided here, from the document. What to do
        // about it is the only thing a model gets a say in.
        let Some((at, asked)) = plan::find_addressed(&blocks, &self.cfg.name) else {
            return Ok(false);
        };
        let ins_id = blocks[at].id.clone();
        if self.handled.contains(&ins_id) {
            return Ok(false);
        }
        self.handled.insert(ins_id.clone());

        let Decision { instruction, route } = self.chooser.choose(&asked, &blocks);
        tracing::info!(
            block = %ins_id,
            asked = %asked,
            decided_by = route.label(),
            live = route.is_live(),
            ?instruction,
            "{}",
            route.detail()
        );
        self.last_route = Some(route);

        match instruction {
            Instruction::Table => self.make_table(&blocks, at)?,
            Instruction::Caption => self.caption_image(&blocks, at)?,
            Instruction::Heading => self.make_heading(&blocks, at)?,
            Instruction::Unknown(what) => {
                let reply = format!(
                    "{}: I can't \"{what}\" yet. I can: table, caption, heading.",
                    self.cfg.name
                );
                let ins = &blocks[at];
                self.edit(
                    &ins.id,
                    wp::splice_text_action(RegionKind::Body, &ins.id, 0, ins.text.chars().count(), &reply),
                )?;
            }
        }
        Ok(true)
    }

    /// Bullets above the instruction become a table; the bullets and the
    /// instruction go away.
    fn make_table(&mut self, blocks: &[Block], at: usize) -> Result<()> {
        let bullets = plan::bullets_above(blocks, at);
        let ins_id = blocks[at].id.clone();
        if bullets.is_empty() {
            let reply = format!("{}: I looked for bullets above this line and found none.", self.cfg.name);
            let n = blocks[at].text.chars().count();
            return self.edit(&ins_id, wp::splice_text_action(RegionKind::Body, &ins_id, 0, n, &reply));
        }
        let lines: Vec<&str> = bullets.iter().map(|&i| blocks[i].text.as_str()).collect();
        let rows = plan::table_cells(&lines);
        let cols = rows.first().map(Vec::len).unwrap_or(1).max(1);
        let last_bullet = blocks[*bullets.last().expect("non-empty")].id.clone();
        let before: HashSet<String> = plan::table_ids(blocks).into_iter().collect();

        self.edit(&last_bullet, wp::insert_table_action(&last_bullet, rows.len(), cols))?;

        // The table's cells were minted inside the action; read them back.
        let fresh = plan::blocks(&self.editor.as_ref().expect("editor").snapshot()?);
        let table_id = plan::table_ids(&fresh)
            .into_iter()
            .find(|t| !before.contains(t))
            .context("the inserted table's cells were not found")?;
        let cells = plan::cells_of(&fresh, &table_id);
        for (row, col, cell_id) in cells {
            let text = rows
                .get(row as usize)
                .and_then(|r| r.get(col as usize))
                .cloned()
                .unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            self.edit(&cell_id, wp::splice_text_action(RegionKind::Body, &cell_id, 0, 0, &text))?;
        }
        let mut gone: Vec<String> = bullets.iter().map(|&i| blocks[i].id.clone()).collect();
        gone.push(ins_id);
        self.edit(&last_bullet, wp::remove_blocks_action(gone))?;
        Ok(())
    }

    /// A caption paragraph under the first image; the instruction goes away.
    fn caption_image(&mut self, blocks: &[Block], at: usize) -> Result<()> {
        let ins_id = blocks[at].id.clone();
        let Some(img) = plan::first_image(blocks) else {
            let reply = format!("{}: there is no image in this document to caption.", self.cfg.name);
            let n = blocks[at].text.chars().count();
            return self.edit(&ins_id, wp::splice_text_action(RegionKind::Body, &ins_id, 0, n, &reply));
        };
        let img_id = blocks[img].id.clone();
        let figure = plan::first_image(blocks).map(|_| 1).unwrap_or(1);
        self.edit(&img_id, wp::insert_paragraph_action(&img_id, &format!("Figure {figure}")))?;
        self.edit(&img_id, wp::remove_blocks_action(vec![ins_id]))?;
        Ok(())
    }

    /// A structural pass over a rough draft: the first line becomes the
    /// title, each section label under it becomes a heading, and the
    /// instruction goes away. One `SetParagraphStyle` per level, so the
    /// sections land together rather than one flicker at a time.
    fn make_heading(&mut self, blocks: &[Block], at: usize) -> Result<()> {
        let ins_id = blocks[at].id.clone();
        let found = plan::structure(blocks, at);
        let Some(title_id) = found.title else {
            return self.edit(&ins_id, wp::remove_blocks_action(vec![ins_id.clone()]));
        };
        self.edit(
            &title_id,
            wp::set_paragraph_style_action(vec![title_id.clone()], "heading", Some(1), None),
        )?;
        if !found.sections.is_empty() {
            let anchor = found.sections[0].clone();
            self.edit(
                &anchor,
                wp::set_paragraph_style_action(found.sections.clone(), "heading", Some(2), None),
            )?;
        }
        tracing::info!(
            sections = found.sections.len(),
            "made the draft a document"
        );
        self.edit(&title_id, wp::remove_blocks_action(vec![ins_id]))?;
        Ok(())
    }

    /// Show the caret on `focus`, apply `action`, broadcast the change, and
    /// pause so the room can see it happen.
    fn edit(&mut self, focus: &str, action: WordProcessorAction) -> Result<()> {
        let caret_pos = self
            .editor
            .as_ref()
            .and_then(|ed| ed.snapshot().ok())
            .and_then(|s| s.blocks.iter().find(|b| b.id == focus).map(|b| b.text.len()))
            .unwrap_or(0);
        self.link.send(&CollabRequest::Presence {
            key: block_key(focus),
            payload: PresencePayload::Caret { pos: caret_pos },
        })?;
        let delta = self
            .editor
            .as_mut()
            .context("no document yet")?
            .apply(&action)?;
        if let Some(data) = delta {
            self.link.send(&CollabRequest::SyncIncremental { data })?;
            tracing::info!(block = %focus, "applied {}", describe(&action));
        } else {
            tracing::warn!(block = %focus, "{} changed nothing", describe(&action));
        }
        std::thread::sleep(self.cfg.pace);
        Ok(())
    }
}

fn describe(action: &WordProcessorAction) -> String {
    match action {
        WordProcessorAction::SpliceText { ins, del, .. } => format!("splice text (+{} -{del})", ins.chars().count()),
        WordProcessorAction::InsertTable { rows, cols, .. } => format!("insert table {rows}x{cols}"),
        WordProcessorAction::InsertParagraph { .. } => "insert paragraph".to_string(),
        WordProcessorAction::RemoveBlocks { blocks } => format!("remove {} block(s)", blocks.len()),
        WordProcessorAction::SetParagraphStyle { kind, .. } => format!("set style {kind}"),
        other => format!("{other:?}"),
    }
}
