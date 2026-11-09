//! Following a person: the view of the person who follows someone moves with them.
//! Whom a view follows is the follower's own word on their live presence, which every roster reads; this keeps only
//! what a view must not repeat — which of that person's places was already brought forward — so a view that shows it
//! stays where it is and a change of place moves it once.

/// A place a person is in: the window of theirs shown here, or the app they work in where no window here shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Window(u64),
    App(String),
}

/// Where one person here is, as the host reads them off its own desk.
#[derive(Clone, Copy, Debug)]
pub struct Place<'a> {
    /// The device they are here as.
    pub id: &'a str,
    /// The window of this desk that shows their record, when it shows one.
    pub tab: Option<u64>,
    /// The app they work in, when their screen says it.
    pub app: Option<&'a str>,
}

/// What following owes the pass that asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing moved since it was last brought forward.
    Rest,
    /// Bring this forward.
    Bring(Target),
    /// The person followed is no longer here: following them is over, and so is their word that they follow.
    Gone,
}

/// A view's following of one person, across passes.
#[derive(Debug, Default)]
pub struct Follow {
    followed: Option<String>,
    brought: Option<Target>,
}

impl Follow {
    /// Whose view this follows now (`None`: nobody's). What was brought forward belonged to the person followed before,
    /// so a change of person forgets it. Answers whether it changed.
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

    /// What this pass owes, as `places` has everyone here: their place is brought forward the first time it is seen and
    /// each time it moves, and a person who left ends it.
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

    fn place<'a>(id: &'a str, tab: Option<u64>, app: Option<&'a str>) -> Place<'a> {
        Place { id, tab, app }
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
}
