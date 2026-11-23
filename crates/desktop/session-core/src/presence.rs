//! Who is here, drawn where they are: every member's pointer over the tiles a host shows, with their
//! name and what they say beside it, and following a person — the view of the person who follows
//! someone moves with them.
//! Whom a view follows is the follower's own word on their live presence, which every roster reads;
//! this keeps only what a view must not repeat — which of that person's places it already brought
//! forward. Where the pointers are, and what arrives beside them, is the same for both hosts, so the
//! page and the native shell draw the live session's members by one rule.

use collab_types::MemberId;
use collab_types::presence::Shown;
use tilo_live::presence::{KEEPALIVE_MS, Tile, tile};
use ui::presence::{CursorChats, PeerCursor, PlacedPointer, PresenceCtx};
use ui::time::Instant;
use ui::tokens::EffectiveMotion;

/// Keep drawing this long after the newest pointer was heard, so it eases to rest instead of
/// stopping mid-move.
const SETTLE_MS: f64 = 250.0;

/// A pointer whose publisher said nothing for this long is no longer shown: a member here restates
/// itself every keepalive, so a longer silence is a device that left before the workspace noticed.
pub const POINTER_STALE_MS: f64 = KEEPALIVE_MS + 1000.0;

/// A person as the bar and People draw them: their name, told apart as everywhere here, and their
/// colour.
pub type Face = (String, (u8, u8, u8));

/// Who points where on a host's tiles now, and what each of them says beside their pointer.
#[derive(Debug, Default)]
pub struct Pointers {
    /// Each pointer as a frame draws it, in the order the board lists its people.
    pub drawn: Vec<PeerCursor>,
    /// The tab each of [`Self::drawn`] is over.
    pub tabs: Vec<u64>,
    /// Keep drawing until then, on the host's presence clock (milliseconds): a pointer still easing
    /// to rest.
    pub motion_until: Option<f64>,
    /// When the next silent pointer leaves the screen (milliseconds), while one will.
    pub leaving: Option<f64>,
    /// A bubble or a reaction is playing: the next frame draws a change.
    pub animating: bool,
}

/// Everyone whose pointer is over one of `tiles` now, as a frame draws them: each in the colour and
/// under the name `presence` knows the person by, where the tile they are over stands, with what
/// they say beside it. `members` is who is here as the board lists them, `now` times the bubble and
/// the reaction the receiver's own clock, and `now_ms` is the same clock in the milliseconds
/// presence is heard on. `faces` names the people here and gives their colour by publisher, as the
/// bar and the People list draw them: a pointer of someone there wears that name and colour.
pub fn pointers<S: std::hash::BuildHasher>(
    members: &[Shown],
    faces: &std::collections::HashMap<String, Face, S>,
    tiles: &[Tile],
    chats: &mut CursorChats,
    presence: &PresenceCtx,
    now: Instant,
    now_ms: f64,
    motion: EffectiveMotion,
) -> Pointers {
    let mut pointers = Pointers::default();
    for member in members {
        let Some(pointer) = &member.pointer else {
            continue;
        };
        let Ok(tab) = pointer.tab.parse::<u64>() else {
            continue;
        };
        let Some(tile) = tile(tiles, tab) else {
            continue;
        };
        if pointer.age_ms > POINTER_STALE_MS {
            continue;
        }
        let heard = now_ms - pointer.age_ms;
        pointers.motion_until = Some(
            pointers
                .motion_until
                .map_or(heard + SETTLE_MS, |asked| asked.max(heard + SETTLE_MS)),
        );
        pointers.leaving = Some(pointers.leaving.map_or(heard + POINTER_STALE_MS, |asked| {
            asked.min(heard + POINTER_STALE_MS)
        }));
        let Some(user) = member.member_id() else {
            continue;
        };
        chats.say(
            &user,
            member
                .chat
                .as_ref()
                .map(|chat| (chat.text.as_str(), chat.sent)),
            now,
        );
        if let Some(reaction) = &member.reaction {
            chats.react(&user, reaction.seq, &reaction.emoji, now);
        }
        let [x, y] = pointer.at;
        let chat = chats.bubble(&user, now, motion);
        let reaction = chats.reaction(&user, now, motion);
        let face = faces.get(&member.publisher);
        let mut cursor = presence.placed_cursor(PlacedPointer {
            user,
            at: tile.point((x, y)),
            name: Some(member.label.clone()),
            activity: member.activity.clone(),
            chat,
            reaction,
        });
        if let Some((label, (r, g, b))) = face {
            cursor.label.clone_from(label);
            cursor.color = ui::vello::peniko::Color::from_rgb8(*r, *g, *b);
        }
        pointers.drawn.push(cursor);
        pointers.tabs.push(tab);
    }
    chats.prune(now);
    pointers.animating = chats.animating(now);
    pointers
}

/// A place a person is in: the window of theirs shown here, or the app they work in where no window here shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Window(u64),
    App(String),
}

/// Where one person here is, as the host reads them off its own shell.
#[derive(Clone, Copy, Debug)]
pub struct Place<'a> {
    /// The device they are here as.
    pub id: &'a str,
    /// The window of this shell that shows their record, when it shows one.
    pub tab: Option<u64>,
    /// The app they work in, when their screen says it.
    pub app: Option<&'a str>,
}

/// What following owes the pass that asks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Step {
    /// Nothing moved since it was last brought forward.
    #[default]
    Rest,
    /// Bring this forward.
    Bring(Target),
    /// The person followed is no longer here: following them is over.
    Gone,
}

/// What one pass of following owes the shell that shows it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Owed {
    /// The view this pass brings forward, if any.
    pub step: Step,
    /// Following ended in this pass: the person's own word says nobody's view is theirs now.
    pub ended: bool,
    /// Whom the workspace's apps are to hear, where they heard something else: the workspace, and
    /// the member (`None`: nobody).
    pub tell: Option<(String, Option<MemberId>)>,
}

/// A view's following of one person, across passes.
#[derive(Debug, Default)]
pub struct Follow {
    followed: Option<String>,
    brought: Option<Target>,
    /// Whom the workspace's apps were told this view follows (`None`: nobody), and in which
    /// workspace: only a change is told again.
    told: Option<(String, MemberId)>,
}

impl Follow {
    /// Whether this pass owes nothing at all: the person follows nobody, and followed nobody
    /// before, so nothing need be read of who is here.
    #[must_use]
    pub fn idle(&self, followed: Option<&str>) -> bool {
        followed.is_none() && self.followed.is_none()
    }

    /// Whose view this follows now (`None`: nobody's); a change of person forgets what was brought forward, so their
    /// place is brought too. Answers whether it changed.
    pub fn follow(&mut self, id: Option<String>) -> bool {
        if self.followed == id {
            return false;
        }
        self.followed = id;
        self.brought = None;
        true
    }

    /// Whose view this follows, as the follower's presence said it.
    #[must_use]
    pub fn followed(&self) -> Option<&str> {
        self.followed.as_deref()
    }

    /// What following owes this pass, from the person's own word (`followed`): where the view goes
    /// as `places` has everyone here (`member` is who they follow, as the apps name them), and whom
    /// the apps of `workspace` are to hear. `working` says the person is doing something of their
    /// own, which takes their view back, except the press that asked to follow (`follow_asked`);
    /// that press owes nothing yet. `None` where the pass owes nothing, and the press' ask stands.
    pub fn owe(
        &mut self,
        followed: Option<String>,
        working: bool,
        follow_asked: bool,
        places: &[Place<'_>],
        member: Option<MemberId>,
        workspace: &str,
    ) -> Option<Owed> {
        if working {
            if follow_asked {
                return None;
            }
            self.follow(None);
            return Some(Owed {
                step: Step::Rest,
                ended: true,
                tell: self.told(workspace, None),
            });
        }
        self.follow(followed);
        let step = self.step(places);
        Some(Owed {
            ended: step == Step::Gone,
            tell: self.told(workspace, member),
            step,
        })
    }

    /// Whom the apps of `workspace` are to hear this view follows, where that differs from what
    /// they were last told. Following ended, the workspace it was told in hears it, wherever the
    /// person looks now.
    fn told(
        &mut self,
        workspace: &str,
        member: Option<MemberId>,
    ) -> Option<(String, Option<MemberId>)> {
        let Some(member) = member else {
            let (workspace, _) = self.told.take()?;
            return Some((workspace, None));
        };
        if workspace.is_empty() {
            return None;
        }
        if self
            .told
            .as_ref()
            .is_some_and(|(was, who)| *was == workspace && *who == member)
        {
            return None;
        }
        self.told = Some((workspace.to_owned(), member.clone()));
        Some((workspace.to_owned(), Some(member)))
    }

    /// What this pass owes, as `places` has everyone here: their place is brought forward the first time it is seen and
    /// each time it moves, and someone who left ends it.
    pub fn step(&mut self, places: &[Place<'_>]) -> Step {
        let Some(followed) = self.followed.as_deref() else {
            return Step::Rest;
        };
        let Some(person) = places.iter().find(|person| person.id == followed) else {
            self.followed = None;
            self.brought = None;
            return Step::Gone;
        };
        let Some(want) = person
            .tab
            .map(Target::Window)
            .or_else(|| person.app.map(|app| Target::App(app.to_owned())))
        else {
            return Step::Rest;
        };
        if self.brought.as_ref() == Some(&want) {
            return Step::Rest;
        }
        self.brought = Some(want.clone());
        Step::Bring(want)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::presence::{ShellChat, ShellReaction, ShownPoint};
    fn place<'a>(id: &'a str, tab: Option<u64>, app: Option<&'a str>) -> Place<'a> {
        Place { id, tab, app }
    }

    /// Someone here whose pointer the board placed over `tab`, `age_ms` after they said it.
    fn pointing(publisher: &str, label: &str, tab: u64, age_ms: f64) -> Shown {
        Shown {
            publisher: publisher.to_owned(),
            user: Some(format!("{publisher}-person")),
            label: label.to_owned(),
            pointer: Some(ShownPoint {
                tab: tab.to_string(),
                at: [10.0, 20.0],
                age_ms,
            }),
            ..Shown::default()
        }
    }

    /// A person's pointer wears the name and colour of their face on the bar and in People, never
    /// the key their presence was said under.
    #[test]
    fn a_pointer_wears_its_persons_face() {
        let tiles = [Tile::new(1, 100.0, 50.0, 300.0, 200.0)];
        let key = "ab".repeat(32);
        let member = pointing("device", &key, 1, 10.0);
        let faces = std::collections::HashMap::from([(
            "device".to_owned(),
            ("Bo".to_owned(), (0x38, 0xb8, 0xf6)),
        )]);
        let drawn = pointers(&mut tilo_ui::DrawCtx::new(std::slice::from_ref(&member), &faces, &tiles, &mut CursorChats::default(), PresenceCtx::empty().draw_env()), Instant::now(), 1_000.0, EffectiveMotion::default())
        .drawn;
        assert_eq!(drawn[0].label, "Bo");
        assert_eq!(
            drawn[0].color,
            ui::vello::peniko::Color::from_rgb8(0x38, 0xb8, 0xf6)
        );
        let unnamed = pointers(&mut tilo_ui::DrawCtx::new(&[member], &std::collections::HashMap::new(), &tiles, &mut CursorChats::default(), PresenceCtx::empty().draw_env()), Instant::now(), 1_000.0, EffectiveMotion::default())
        .drawn;
        assert_eq!(unnamed[0].label, ui::presence::unnamed_person_label());
    }

    /// A pointer is drawn where the tile it is over stands, under the name presence knows the
    /// person by, with what they say beside it and the reaction they gave.
    #[test]
    fn a_pointer_is_drawn_where_its_tile_stands_with_what_is_said_beside_it() {
        let tiles = [Tile::new(1, 100.0, 50.0, 300.0, 200.0)];
        let mut member = pointing("ada", "Ada", 1, 100.0);
        member.chat = Some(ShellChat {
            text: "look here".into(),
            sent: true,
        });
        member.reaction = Some(ShellReaction {
            seq: 3,
            emoji: "🎉".into(),
        });
        let now = Instant::now();
        let mut chats = CursorChats::default();
        // What they say is heard on one pass and drawn on the next, once it has come in.
        let _ = pointers(&mut tilo_ui::DrawCtx::new(&[member.clone()], &std::collections::HashMap::new(), &tiles, &mut chats, PresenceCtx::empty().draw_env()), now, 1_000.0, EffectiveMotion::default());
        let pointers = pointers(&mut tilo_ui::DrawCtx::new(&[member], &std::collections::HashMap::new(), &tiles, &mut chats, PresenceCtx::empty().draw_env()), now + std::time::Duration::from_millis(200), 1_200.0, EffectiveMotion::default());
        assert_eq!(pointers.tabs, vec![1]);
        let [cursor] = pointers.drawn.as_slice() else {
            panic!("one pointer");
        };
        assert_eq!(
            cursor.pos,
            (110.0, 70.0),
            "the tile's own corner plus the point"
        );
        assert_eq!(cursor.label, "Ada");
        assert_eq!(
            cursor.chat.as_ref().map(|chat| chat.text.as_str()),
            Some("look here")
        );
        assert_eq!(
            cursor
                .reaction
                .as_ref()
                .map(|reaction| reaction.emoji.as_str()),
            Some("🎉")
        );
        assert!(
            pointers.animating,
            "the bubble and the reaction are playing"
        );
        let heard = 1_200.0 - 100.0;
        assert_eq!(pointers.motion_until, Some(heard + SETTLE_MS));
        assert_eq!(pointers.leaving, Some(heard + POINTER_STALE_MS));
    }

    /// A silent pointer leaves the screen at the end of its keepalive window, and one over no tile
    /// this host shows is not drawn at all: a pointer says where someone is here, never where they
    /// were.
    #[test]
    fn a_silent_pointer_leaves_and_one_over_no_tile_is_not_drawn() {
        let tiles = [Tile::new(1, 0.0, 0.0, 100.0, 100.0)];
        let mut chats = CursorChats::default();
        let quiet = pointers(&mut tilo_ui::DrawCtx::new(&[pointing("ada", "Ada", 1, POINTER_STALE_MS + 1.0)], &std::collections::HashMap::new(), &tiles, &mut chats, PresenceCtx::empty().draw_env()), Instant::now(), 5_000.0, EffectiveMotion::default());
        assert!(quiet.drawn.is_empty());
        assert_eq!(quiet.leaving, None, "it already left");
        let elsewhere = pointers(&mut tilo_ui::DrawCtx::new(&[pointing("cy", "Cy", 9, 0.0)], &std::collections::HashMap::new(), &tiles, &mut chats, PresenceCtx::empty().draw_env()), Instant::now(), 5_000.0, EffectiveMotion::default());
        assert!(elsewhere.drawn.is_empty(), "the shell does not show tab 9");
    }

    /// Their window comes forward once; the same place said again moves nothing, and a change of place moves it again.
    #[test]
    fn a_person_is_brought_forward_once_per_place() {
        let mut follow = Follow::default();
        assert_eq!(
            follow.step(&[]),
            Step::Rest,
            "nobody followed, nothing owed"
        );
        assert!(follow.follow(Some("ada".into())));
        assert!(
            !follow.follow(Some("ada".into())),
            "the same person is no change"
        );
        let here = [place("cy", Some(9), None), place("ada", Some(7), None)];
        assert_eq!(follow.step(&here), Step::Bring(Target::Window(7)));
        assert_eq!(follow.step(&here), Step::Rest, "the view already shows it");
        let moved = [place("ada", Some(8), None)];
        assert_eq!(follow.step(&moved), Step::Bring(Target::Window(8)));
    }

    /// Where no window here shows their record, their app is brought forward; a place nobody knows yet owes nothing.
    #[test]
    fn an_app_stands_in_for_a_window_and_an_unknown_place_owes_nothing() {
        let mut follow = Follow::default();
        follow.follow(Some("ada".into()));
        assert_eq!(follow.step(&[place("ada", None, None)]), Step::Rest);
        let app = [place("ada", None, Some("so.tilo.notes"))];
        assert_eq!(
            follow.step(&app),
            Step::Bring(Target::App("so.tilo.notes".into()))
        );
        assert_eq!(follow.step(&app), Step::Rest);
    }

    /// Following the one they left, and following someone else, both start over.
    #[test]
    fn a_person_who_left_ends_it_and_another_begins_afresh() {
        let mut follow = Follow::default();
        follow.follow(Some("ada".into()));
        assert_eq!(
            follow.step(&[place("ada", Some(7), None)]),
            Step::Bring(Target::Window(7))
        );
        assert_eq!(follow.step(&[place("cy", Some(9), None)]), Step::Gone);
        assert_eq!(follow.followed(), None, "and their word says so no longer");

        follow.follow(Some("cy".into()));
        assert_eq!(
            follow.step(&[place("cy", Some(7), None)]),
            Step::Bring(Target::Window(7)),
            "a place the last person was also in is theirs now"
        );
    }

    fn member(person: &str) -> MemberId {
        MemberId::person(person)
    }

    /// Following someone brings their place forward and tells the workspace's apps whose view is
    /// this person's, once; stopping tells the same workspace that it is nobody's.
    #[test]
    fn following_tells_the_apps_whose_view_is_theirs() {
        let mut follow = Follow::default();
        let here = [place("ada", Some(7), None), place("cy", Some(9), None)];
        let ada = Some(member("ada-person"));
        let owed = follow
            .owe(Some("ada".into()), false, false, &here, ada.clone(), "work")
            .expect("the pass owes following");
        assert_eq!(owed.step, Step::Bring(Target::Window(7)));
        assert_eq!(owed.tell, Some(("work".into(), ada.clone())));
        assert!(!owed.ended);
        let again = follow
            .owe(Some("ada".into()), false, false, &here, ada, "work")
            .expect("the pass owes following");
        assert_eq!(again.step, Step::Rest);
        assert_eq!(again.tell, None, "the apps were told, and nothing moved");
        let stopped = follow
            .owe(None, false, false, &here, None, "work")
            .expect("stopping is owed");
        assert_eq!(stopped.tell, Some(("work".into(), None)));
        assert!(!stopped.ended, "the person's own word ended it");
        assert_eq!(
            follow
                .owe(None, false, false, &here, None, "work")
                .map(|owed| owed.tell),
            Some(None),
            "nothing more is told"
        );
    }

    /// The person doing something of their own takes their view back, and the apps hear nobody is
    /// followed; the press that asked to follow is not them working.
    #[test]
    fn the_persons_own_work_ends_following_and_the_press_that_asked_does_not() {
        let mut follow = Follow::default();
        let here = [place("ada", Some(7), None)];
        let ada = Some(member("ada-person"));
        follow.owe(Some("ada".into()), false, false, &here, ada, "work");
        assert_eq!(
            follow.owe(Some("ada".into()), true, true, &here, None, "work"),
            None,
            "the press that asked is still in flight"
        );
        let ended = follow
            .owe(Some("ada".into()), true, false, &here, None, "work")
            .expect("their work ends following");
        assert!(ended.ended);
        assert_eq!(ended.step, Step::Rest);
        assert_eq!(ended.tell, Some(("work".into(), None)));
    }

    /// Following someone who left ends it, and the workspace the apps were told in hears that.
    #[test]
    fn a_person_who_left_tells_the_workspace_they_were_followed_in() {
        let mut follow = Follow::default();
        follow.owe(
            Some("ada".into()),
            false,
            false,
            &[place("ada", Some(7), None)],
            Some(member("ada-person")),
            "work",
        );
        let gone = follow
            .owe(Some("ada".into()), false, false, &[], None, "elsewhere")
            .expect("the pass owes it");
        assert!(gone.ended);
        assert_eq!(gone.step, Step::Gone);
        assert_eq!(
            gone.tell,
            Some(("work".into(), None)),
            "the workspace it was followed in hears it, not the one looked at now"
        );
    }
}
