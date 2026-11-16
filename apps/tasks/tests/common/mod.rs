#![allow(dead_code, reason = "each test binary reads what it needs of these")]

//! What the tests read of the issues: the parts the app's store keeps.

use tasks::App;
use ui::prelude::*;

/// The issues the app's parts keep, as its store holds them.
pub struct Held(Vec<(u64, std::rc::Rc<tasks::IssuePart>)>);

impl Held {
    pub fn issues(&self) -> tasks::Issues<'_> {
        tasks::Issues::of(&self.0)
    }

    pub fn get(&self, id: &Id) -> Option<tasks::model::Issue> {
        self.issues().get(id).cloned()
    }

    pub fn ids(&self) -> Vec<Id> {
        self.issues().iter().map(|(id, _)| id.clone()).collect()
    }

    pub fn iter(&self) -> std::vec::IntoIter<(Id, tasks::model::Issue)> {
        self.issues()
            .iter()
            .map(|(id, issue)| (id.clone(), issue.clone()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub fn values(&self) -> impl Iterator<Item = tasks::model::Issue> {
        self.iter().map(|(_, issue)| issue)
    }

    pub fn len(&self) -> usize {
        self.issues().len()
    }

    pub fn is_empty(&self) -> bool {
        self.issues().is_empty()
    }

    pub fn by_number(&self, number: u64) -> Option<(Id, tasks::model::Issue)> {
        self.issues()
            .by_number(number)
            .map(|(id, issue)| (id.clone(), issue.clone()))
    }
}

pub fn held(app: &TestApp<App>) -> Held {
    Held(app.chain_parts::<tasks::IssuePart>())
}

/// Where the issues' actions are published: the chain of their parts.
pub fn chain() -> String {
    ui::record::chain_resource_id(&tasks::part::chain())
}
