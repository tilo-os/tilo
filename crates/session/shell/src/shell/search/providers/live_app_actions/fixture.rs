//! A live app actions provider over one directory page, for tests elsewhere
//! in the shell.

use std::rc::Rc;

use tilo_actions::remote::CataloguePage;

use super::LiveAppActionsProvider;
use crate::shell::collab_broker::AppActionBroker;
use crate::shell::search::SearchProvider;

/// A provider whose directory already holds `page` from `peer`, as `actor`
/// sees it.
pub(crate) fn fixture_with_directory_page(
    broker: AppActionBroker,
    peer: u64,
    actor: String,
    page: CataloguePage,
) -> Rc<dyn SearchProvider> {
    let provider = Rc::new(LiveAppActionsProvider::new_with_fixture_identity(broker));
    provider
        .directory_pages_mut()
        .insert((peer, actor, String::new(), Vec::new()), page);
    provider
}
