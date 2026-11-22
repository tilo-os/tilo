//! Who publishes in the caller's workspace: every provider the broker lists,
//! apps' windows and the host's own services alike.

use super::{ActionProvider, McpServer, Value};

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// The application windows among [`Self::providers`], by peer id.
    pub(super) async fn instances(&self) -> Result<(String, Vec<(u64, Value)>), String> {
        let (revision, listed) = self.tree.providers().await?;
        let apps = listed
            .into_iter()
            .filter_map(|(provider, entry)| Some((provider.parse::<u64>().ok()?, entry)))
            .collect::<Vec<_>>();
        *self.seen() = apps
            .iter()
            .map(|(peer, app)| (*peer, app["app_id"].as_str().map(str::to_owned)))
            .collect();
        Ok((revision, apps))
    }
}
