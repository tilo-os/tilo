//! Existing-resource metadata through the current native owner and room host.
use std::sync::Arc;
use tilo_action_broker::services::{
    NativeDocumentReadContext, NativeDocumentReads as _, NativeResourceViewFuture,
};

pub(super) struct NativeResourceViews {
    pub(super) owner: Arc<super::NativeOwnerCommitService>,
    pub(super) network: Arc<super::super::network::ShellNetwork>,
}

impl tilo_action_broker::services::NativeResourceViews for NativeResourceViews {
    fn query(
        &self,
        context: NativeDocumentReadContext,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> NativeResourceViewFuture {
        let owner = self.owner.clone();
        let network = self.network.clone();
        Box::pin(async move {
            let unavailable =
                |reason| tilo_actions::remote::ActionHostError::Unavailable { reason };
            owner.authorize(&context).map_err(unavailable)?;
            let room = super::super::state::RoomId::parse(&context.route().cluster_id)
                .ok_or(tilo_actions::remote::ActionHostError::NotPermitted)?;
            let page = network
                .discover_resource_view(room, query)
                .await
                .map_err(unavailable)?;
            owner.authorize(&context).map_err(unavailable)?;
            Ok(page)
        })
    }
}
