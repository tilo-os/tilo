//! Usage: how long the agents' Linux machines ran. Nothing counts it yet;
//! the place says what will show here once something does.

use ui::icons::Icon;
use ui::prelude::*;

use crate::{page, tr};

pub(crate) fn view() -> Element {
    page(vec![Box::new(
        EmptyState::new(tr("console-usage-empty"))
            .with_message(tr("console-usage-empty-message"))
            .with_icon(Icon::Gauge)
            .with_id("console-usage-empty"),
    )])
}
