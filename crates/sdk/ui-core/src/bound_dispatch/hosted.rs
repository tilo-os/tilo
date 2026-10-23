//! Controls bound to another provider's actions
//! ([`tilo_actions::ActionSpec::run_by`]): the component neither publishes
//! nor runs them, and its host invokes them at that provider.

/// An invocation of another provider's action a view bound its control to,
/// resolved and confirmed here, for the host to invoke at that provider.
#[derive(Clone, Debug)]
pub(crate) struct HostedInvocation {
    pub(crate) provider: String,
    /// The action as the provider publishes it.
    pub(crate) address: tilo_actions::ActionAddress,
    pub(crate) argument: tilo_actions::remote::InvokeArgument,
    /// Who invoked it here.
    pub(crate) actor: String,
    /// The person confirmed it here, where its value asks first.
    pub(crate) confirmed: bool,
}

impl HostedInvocation {
    /// The invocation at `address` of `spec`, when another provider runs
    /// it, with its typed `arguments` or text `argument`.
    pub(crate) fn of(
        spec: &tilo_actions::ActionSpec,
        address: &tilo_actions::ActionAddress,
        arguments: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
        actor: &str,
        confirmed: bool,
    ) -> Option<Self> {
        let run = spec.run_by_provider()?;
        let resource = run.resource.as_deref().unwrap_or(&address.resource);
        let (action, argument) = match &run.invokes {
            // Offered here as another: the provider's own action, with what
            // this one fixes and what was given beside it.
            Some((action, fixed)) => {
                let mut arguments = arguments.cloned().unwrap_or_default();
                arguments.extend(fixed.clone());
                (
                    action.as_str(),
                    tilo_actions::remote::InvokeArgument::Json(arguments),
                )
            }
            None => (
                address.action.as_str(),
                match (arguments, argument) {
                    (Some(arguments), _) => {
                        tilo_actions::remote::InvokeArgument::Json(arguments.clone())
                    }
                    (None, Some(text)) => {
                        tilo_actions::remote::InvokeArgument::Text(text.to_owned())
                    }
                    (None, None) => tilo_actions::remote::InvokeArgument::None,
                },
            ),
        };
        Some(Self {
            provider: run.provider.clone(),
            address: tilo_actions::ActionAddress::new(resource, action),
            argument,
            actor: actor.to_owned(),
            confirmed,
        })
    }
}

/// `resource` as its component publishes it: without the actions another
/// provider runs, which that provider publishes itself. `None` where nothing
/// of it is left to publish.
pub(crate) fn published_here(
    mut resource: tilo_actions::Resource,
) -> Option<tilo_actions::Resource> {
    let hosted = resource
        .actions
        .iter()
        .any(|spec| spec.provider().is_some());
    resource.actions.retain(|spec| spec.provider().is_none());
    let children = resource.children.len();
    resource.children = std::mem::take(&mut resource.children)
        .into_iter()
        .filter_map(published_here)
        .collect();
    let emptied = (hosted || resource.children.len() < children)
        && resource.actions.is_empty()
        && resource.children.is_empty();
    (!emptied).then_some(resource)
}
