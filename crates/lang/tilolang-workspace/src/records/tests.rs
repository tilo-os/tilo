//! tilolang Tasks' record as this device's own roles read and change it ([`Opened`]), over a
//! store held in this process ([`LocalPort`]): the same host, packages, publication and paging
//! the workspace service runs for a record no window has open, with no workspace service around
//! it.
//!
//! What runs here is what an agent or a person's `tilo do` reaches through the dispatch: a task
//! made through a published action, the rows the record lists a level at a time, and what an
//! action on it costs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "in a test these are the assertion, not a hazard"
)]

mod action_cost;
mod members_root;
mod task_listing;
mod task_reads;

use std::rc::Rc;

use serde_json::Value as Json;
use tilo_actions::{ActionAddress, ActionContext, ActionInvoker, ActionOutput};
use tilolang_tilo::publish::{package_resource_id, resource_id};

use super::{LocalPort, Opened, ResourceStore, built_in, member};

/// The app: tilolang Tasks, one of the packages built into this build.
const APP: &str = "so.tilo.tasks";

/// The person whose workspace it is, and who acts in it.
const PERSON: &str = "ada";

/// Tasks' record of a workspace, opened as the workspace service opens it.
struct Tasks {
    opened: Opened,
    /// The published id of the workspace's tasks: the resource every task is listed under.
    root: String,
}

impl Tasks {
    /// An empty workspace's record, its tasks made as a launcher opening the app makes them.
    fn founded() -> Self {
        let packages = built_in(APP)
            .expect("the built-in packages compile")
            .expect("Tasks is built into this build");
        let store = ResourceStore::new(Rc::new(LocalPort::in_memory()));
        let mut opened = Opened::open(
            store,
            &packages,
            member(tilo_actions::MemberId::person(PERSON)),
            None,
        )
        .expect("the record opens");
        let root = opened.open_root().expect("the tasks are made");
        Self {
            root: resource_id(APP, &root.id_string()),
            opened,
        }
    }

    /// Run the published action `address` with `arguments` as the person, as the dispatch hands
    /// it to the record: what it answered.
    fn invoke(&mut self, address: &ActionAddress, arguments: &Json) -> ActionOutput {
        let Json::Object(arguments) = arguments else {
            panic!("an action's arguments are named: {arguments}");
        };
        self.opened
            .invoke_published(
                address,
                arguments,
                &ActionInvoker::person(PERSON),
                &ActionContext::default(),
            )
            .unwrap_or_else(|error| panic!("{address:?} runs: {error:?}"))
    }

    /// Add the task `title` through the tasks' own `new_task`: the published id of the task it
    /// made.
    fn new_task(&mut self, title: &str) -> String {
        let address = ActionAddress::new(self.root.clone(), "new_task");
        let output = self.invoke(&address, &serde_json::json!({ "title": title }));
        output
            .resources
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("new_task names the task it made: {output:?}"))
    }

    /// Add every task of `titles` at once through the package's `new_tasks`, as an import lands a
    /// list of them.
    fn new_tasks(&mut self, titles: &[String]) {
        let address = ActionAddress::new(package_resource_id(APP), "new_tasks");
        self.invoke(&address, &serde_json::json!({ "titles": titles }));
    }

    /// The rows the record lists under its tasks.
    fn under_tasks(&self) -> usize {
        self.opened
            .rows()
            .iter()
            .filter(|row| row.parent_resource == self.root)
            .count()
    }
}
