//! Who publishes in the caller's room: every provider the broker lists,
//! apps' windows and the host's own services alike.

use super::{ActionHost, McpServer, Value, instances_query, json};

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// Every publisher this caller may use in its room, each by its provider
    /// id with what it is: an application's window (a numeric peer, with
    /// its record) or one of the host's own services (`tilo.desktop`), by
    /// the app id it answers for. Host services first, then windows oldest
    /// first, so the newest window of an app is its last entry.
    pub(super) async fn providers(&self) -> Result<(String, Vec<(String, Value)>), String> {
        let page = self.ask(instances_query()).await?;
        let mut listed = page
            .providers
            .into_iter()
            .filter(|provider| provider.browsable && provider.room == self.room)
            .map(|provider| {
                let peer = provider.provider.parse::<u64>().ok();
                let mut entry = json!({
                    "provider": provider.provider,
                    "app_id": provider.app_id,
                    "record_id": provider.record_id,
                });
                if let (Some(peer), Some(fields)) = (peer, entry.as_object_mut()) {
                    fields.insert("peer_id".to_owned(), json!(peer));
                    fields.insert("uri".to_owned(), json!(format!("tilo://apps/{peer}")));
                }
                (provider.provider, entry)
            })
            .collect::<Vec<_>>();
        listed.sort_by_key(|(provider, _)| (provider.parse::<u64>().ok(), provider.clone()));
        Ok((page.revision, listed))
    }
    /// The application windows among [`Self::providers`], by peer id.
    pub(super) async fn instances(&self) -> Result<(String, Vec<(u64, Value)>), String> {
        let (revision, listed) = self.providers().await?;
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
