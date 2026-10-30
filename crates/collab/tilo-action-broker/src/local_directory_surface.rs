// The fixture's application with a live surface beside its directory: as an
// app runs, listed and browsable at once. Included into the broker's test
// support, beside `LocalDirectoryFixture`.

impl LocalDirectoryFixture {
    /// Publish, as the fixture's application, one action of `resource`
    /// taking `params` under `contract` on its live surface beside its
    /// directory, and subscribe to its invocations: an app as it runs,
    /// listed and browsable at once. The surface lives as long as the
    /// channel returned with its invocations.
    pub async fn publish_surface_action(
        &self,
        (resource, action): (&str, &str),
        params: &[tilo_actions::ActionParam],
        contract: &tilo_actions::ActionContract,
    ) -> (
        runtime_collab::ChannelHandle,
        runtime_collab::InvocationStream,
    ) {
        let channel = self.invocation_channel();
        let action = runtime_collab::ExportedAction {
            semantic_identity: None,
            resource: resource.into(),
            action: action.into(),
            resource_kind: "note".into(),
            resource_label: "Notes".into(),
            action_label: "Append".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: params.to_vec(),
            contract: contract.clone(),
            input: runtime_collab::ExportedInput::None,
            view: None,
        };
        channel
            .publish_actions(&[action])
            .await
            .expect("the surface is published");
        let stream = channel
            .subscribe_stable_invocations()
            .await
            .expect("subscribed");
        (channel, stream)
    }
}
