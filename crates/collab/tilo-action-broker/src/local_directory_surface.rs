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
    ) -> (channel_handle::Client, invocation_stream::Client) {
        let channel = self.invocation_channel();
        let mut publish = channel.publish_actions_request();
        publish.get().set_app_id(&self.route.app_id);
        {
            let mut row = publish.get().init_actions(1).get(0);
            row.set_resource(resource);
            row.set_action(action);
            row.set_resource_kind("note");
            row.set_resource_label("Notes");
            row.set_action_label("Append");
            row.set_operation_kind("effect");
            row.set_params_json(serde_json::to_string(params).expect("params encode"));
            row.set_contract_json(serde_json::to_string(contract).expect("contract encodes"));
        }
        publish
            .send()
            .promise
            .await
            .expect("the surface is published");
        let mut subscribe = channel.subscribe_invocations_request();
        subscribe.get().set_stable_invocation_ids(true);
        let stream = subscribe
            .send()
            .promise
            .await
            .expect("subscribed")
            .get()
            .expect("subscribed")
            .get_stream()
            .expect("an invocation stream");
        (channel, stream)
    }
}
