//! Walks of a widget tree that read what its widgets publish (resources,
//! the items past a page, search providers, an action directory's pages),
//! each read under the component the widget belongs to.

#[cfg(feature = "model")]
use tilo_ui_base::widget::ActionDirectoryResourcePage;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{widget::component_walk::Binding, widget::component_walk::Entry, widget::component_walk::binding_of, widget::component_walk::enter, widget::component_walk::has_separate_document_owner};
use automerge::AutoCommit;

/// Where a walk stands at one widget: whether the widget is a component's
/// boundary, the component it reads under, and that component's scope held.
pub(crate) struct Crossing {
    /// The widget hosts a component: what it publishes is the component's,
    /// read below it.
    pub(crate) boundary: bool,
    /// The component the widget belongs to, its own or the one it is inside.
    pub(crate) binding: Option<Binding>,
    _entry: Entry,
}

/// Step onto `widget`, inside `inherited`'s component. `None` when the walk
/// must leave the widget's subtree: it owns a document of its own no component
/// answers for, or its component has closed.
pub(crate) fn cross(widget: &dyn Widget, inherited: Option<Binding>) -> Option<Crossing> {
    let own = binding_of(widget);
    if own.is_none() && has_separate_document_owner(widget) {
        return None;
    }
    let boundary = own.is_some();
    let binding = own.or(inherited);
    let entry = enter(binding.as_ref())?;
    Some(Crossing {
        boundary,
        binding,
        _entry: entry,
    })
}

impl Crossing {
    /// `read` of `doc` as the widget holds it: its component's document, else
    /// the window's. `None` when the component's document cannot be read now.
    #[cfg_attr(
        not(feature = "model"),
        expect(
            clippy::unreachable,
            reason = "without the model no widget answers a component scope"
        )
    )]
    pub(crate) fn read<T>(
        &self,
        doc: &AutoCommit,
        read: impl FnOnce(&AutoCommit) -> T,
    ) -> Option<T> {
        match &self.binding {
            #[cfg(feature = "model")]
            Some(binding) => crate::bound_dispatch::callback_binding_of(&**binding)
                .read_document(doc, |doc, _| read(doc))
                .ok(),
            #[cfg(not(feature = "model"))]
            Some(_) => unreachable!("only the model's callback bindings are component scopes"),
            None => Some(read(doc)),
        }
    }
}

/// Collect resources from every retained widget without requiring container
/// authors to remember to forward their children's declarations. Resource
/// ownership stays local to the widget that implements it; composition is a
/// host property, like action-binding and accessibility traversal.
///
/// Inside a component only its root (the widget its boundary hosts)
/// publishes document actions: it speaks for the resource. What the rest of
/// the component's tree publishes is kept to views and effects.
pub(crate) fn collect_widget_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
    resources: &mut Vec<tilo_actions::ResourceSpec>,
) {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        inherited: Option<Binding>,
        root: bool,
        resources: &mut Vec<tilo_actions::ResourceSpec>,
        taken: &mut tilo_ui_base::widget::resources::TakenResources,
    ) {
        let Some(crossing) = cross(widget, inherited) else {
            return;
        };
        if !crossing.boundary {
            let found = crossing
                .read(doc, |doc| tilo_ui_base::widget::resources::resources(widget, doc))
                .unwrap_or_default();
            let found = if root || crossing.binding.is_none() {
                found
            } else {
                found
                    .into_iter()
                    .map(tilo_ui_base::widget::resources::outside_document)
                    .collect()
            };
            let fresh: Vec<_> = found
                .into_iter()
                .filter(|resource| !taken.holds(resource.id.as_str()))
                .collect();
            for resource in &fresh {
                taken.take(resource.id.as_str());
            }
            resources.extend(fresh);
        }
        let below = crossing.boundary;
        widget.for_each_child(&mut |child| {
            visit(
                child,
                doc,
                crossing.binding.clone(),
                below,
                resources,
                taken,
            );
        });
    }
    visit(
        widget,
        doc,
        None,
        true,
        resources,
        &mut tilo_ui_base::widget::resources::TakenResources::default(),
    );
}

/// The resource `id` in `widget`'s subtree, resolved by the widget that publishes it rather
/// than by building every resource the subtree holds: [`collect_widget_resources`] for one
/// address.
#[must_use]
pub fn find_resource(
    widget: &dyn Widget,
    doc: &AutoCommit,
    id: &str,
) -> Option<tilo_actions::ResourceSpec> {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        inherited: Option<Binding>,
        root: bool,
        id: &str,
    ) -> Option<tilo_actions::ResourceSpec> {
        let crossing = cross(widget, inherited)?;
        if !crossing.boundary {
            let found = crossing
                .read(doc, |doc| {
                    tilo_ui_base::widget::resources::resource(widget, doc, id)
                })
                .flatten();
            if let Some(found) = found {
                return Some(if root || crossing.binding.is_none() {
                    found
                } else {
                    tilo_ui_base::widget::resources::outside_document(found)
                });
            }
        }
        let below = crossing.boundary;
        let mut found = None;
        widget.for_each_child(&mut |child| {
            if found.is_none() {
                found = visit(child, doc, crossing.binding.clone(), below, id);
            }
        });
        found
    }
    visit(widget, doc, None, true, id)
}

/// The item resources `widget`'s subtree leaves out of what it publishes
/// past each collection's published page
/// ([`tilo_ui_base::widget::resources::PublishesResources::resources_past_page`]): those of
/// the first widget that names any, read as [`find_resource`] reads them.
pub fn resources_past_page(
    widget: &dyn Widget,
    doc: &AutoCommit,
) -> Vec<tilo_actions::ResourceSpec> {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        inherited: Option<Binding>,
    ) -> Vec<tilo_actions::ResourceSpec> {
        let Some(crossing) = cross(widget, inherited) else {
            return Vec::new();
        };
        if !crossing.boundary {
            let found = crossing
                .read(doc, |doc| {
                    tilo_ui_base::widget::resources::resources_past_page(widget, doc)
                })
                .unwrap_or_default();
            if !found.is_empty() {
                return found;
            }
        }
        let mut found = Vec::new();
        widget.for_each_child(&mut |child| {
            if found.is_empty() {
                found = visit(child, doc, crossing.binding.clone());
            }
        });
        found
    }
    visit(widget, doc, None)
}

/// [`tilo_ui_base::widget::resources::past_page_len`] of `widget`'s subtree: how many items
/// the published pages leave out, counted without building them.
pub fn past_page_len(widget: &dyn Widget, doc: &AutoCommit) -> usize {
    visit_past_page(widget, doc, None, &mut |widget, doc| {
        let len = tilo_ui_base::widget::resources::past_page_len(widget, doc);
        (len > 0).then_some(len)
    })
    .unwrap_or(0)
}

/// [`tilo_ui_base::widget::resources::past_page_labels`] of `widget`'s subtree: the items the
/// published pages leave out, by id and label, without building them.
pub fn past_page_labels(widget: &dyn Widget, doc: &AutoCommit) -> Vec<(String, String)> {
    visit_past_page(widget, doc, None, &mut |widget, doc| {
        let labels = tilo_ui_base::widget::resources::past_page_labels(widget, doc);
        (!labels.is_empty()).then_some(labels)
    })
    .unwrap_or_default()
}

/// [`tilo_ui_base::widget::resources::past_page_range`] of `widget`'s subtree: the items the
/// published pages leave out from `offset` on, built alone.
pub fn past_page_range(
    widget: &dyn Widget,
    doc: &AutoCommit,
    offset: usize,
    take: usize,
) -> Vec<tilo_actions::ResourceSpec> {
    visit_past_page(widget, doc, None, &mut |widget, doc| {
        if tilo_ui_base::widget::resources::past_page_len(widget, doc) == 0 {
            return None;
        }
        Some(tilo_ui_base::widget::resources::past_page_range(
            widget, doc, offset, take,
        ))
    })
    .unwrap_or_default()
}

/// The first widget in `widget`'s subtree that publishes items past its
/// published pages, as `ask` reads it: where a listing's past-page items
/// stand, found without building them all.
fn visit_past_page<T>(
    widget: &dyn Widget,
    doc: &AutoCommit,
    inherited: Option<Binding>,
    ask: &mut dyn FnMut(&dyn Widget, &AutoCommit) -> Option<T>,
) -> Option<T> {
    let crossing = cross(widget, inherited)?;
    if !crossing.boundary {
        let found = crossing.read(doc, |doc| ask(widget, doc)).flatten();
        if found.is_some() {
            return found;
        }
    }
    let mut found = None;
    widget.for_each_child(&mut |child| {
        if found.is_none() {
            found = visit_past_page(child, doc, crossing.binding.clone(), ask);
        }
    });
    found
}

/// Every resource `widget`'s subtree publishes, as the host's action
/// catalogue collects them. A host that embeds components under its own
/// root (a shell surface) reads what its search offers through this.
pub fn published_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
) -> Vec<tilo_actions::ResourceSpec> {
    let mut resources = Vec::new();
    collect_widget_resources(widget, doc, &mut resources);
    resources
}

/// Every search provider the components in `widget`'s subtree publish, each
/// once, as weak proxies the components keep owning.
#[cfg(feature = "model")]
pub fn published_search_providers(
    widget: &dyn Widget,
) -> Vec<Box<dyn crate::search::SearchProvider>> {
    super::search_providers(widget)
}

/// Test traversal probe; production callers retain bound provenance.
#[cfg(test)]
#[cfg(feature = "model")]
pub(crate) fn resolve_widget_resource(
    widget: &dyn Widget,
    doc: &AutoCommit,
    id: &str,
) -> Option<tilo_actions::ResourceSpec> {
    resolve_bound_resource(widget, doc, None, &mut |widget, doc| {
        tilo_ui_base::widget::resources::resource(widget, doc, id)
    })
}

#[cfg(test)]
#[cfg(feature = "model")]
fn resolve_bound_resource<R>(
    widget: &dyn Widget,
    doc: &AutoCommit,
    inherited: Option<crate::bound_dispatch::CallbackBinding>,
    lookup: &mut dyn FnMut(&dyn Widget, &AutoCommit) -> Option<R>,
) -> Option<R> {
    let boundary = crate::bound_dispatch::callback_binding(widget);
    let binding = boundary.clone().or(inherited);
    let _entry = binding
        .as_ref()
        .map(crate::bound_dispatch::CallbackBinding::enter)
        .transpose()
        .ok()?;
    if boundary.is_none() {
        let found = if let Some(binding) = &binding {
            binding
                .read_document(doc, |doc, _| lookup(widget, doc))
                .ok()
                .flatten()
        } else {
            lookup(widget, doc)
        };
        if found.is_some() {
            return found;
        }
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| resolve_bound_resource(child, doc, binding.clone(), lookup))
}

/// Read a bounded page without allowing its retained widget reference or
/// originating binding to escape the traversal.
#[allow(
    clippy::option_option,
    reason = "outer None means end of traversal; inner None means this ordinal has no bounded provider"
)]
#[cfg(feature = "model")]
pub(crate) fn action_directory_page_at(
    widget: &dyn Widget,
    doc: &AutoCommit,
    target: usize,
    cursor: &[u8],
    max_results: usize,
) -> Option<
    Option<(
        ActionDirectoryResourcePage,
        Option<crate::bound_dispatch::CallbackBinding>,
    )>,
> {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        target: usize,
        ordinal: &mut usize,
        inherited: Option<crate::bound_dispatch::CallbackBinding>,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<
        Option<(
            ActionDirectoryResourcePage,
            Option<crate::bound_dispatch::CallbackBinding>,
        )>,
    > {
        let boundary = crate::bound_dispatch::callback_binding(widget);
        let binding = boundary.clone().or(inherited);
        let _entry = binding
            .as_ref()
            .map(crate::bound_dispatch::CallbackBinding::enter)
            .transpose()
            .ok()?;
        if *ordinal == target {
            if boundary.is_some() {
                return Some(None);
            }
            return Some(
                (if let Some(binding) = &binding {
                    binding
                        .read_document(doc, |doc, _| {
                            tilo_ui_base::widget::resources::action_directory_resource_page(
                                widget,
                                doc,
                                cursor,
                                max_results,
                            )
                        })
                        .ok()
                        .flatten()
                } else {
                    tilo_ui_base::widget::resources::action_directory_resource_page(
                        widget,
                        doc,
                        cursor,
                        max_results,
                    )
                })
                .map(|page| (page, binding)),
            );
        }
        *ordinal = ordinal.saturating_add(1);
        widget.children().into_iter().find_map(|child| {
            visit(
                child,
                doc,
                target,
                ordinal,
                binding.clone(),
                cursor,
                max_results,
            )
        })
    }
    visit(widget, doc, target, &mut 0, None, cursor, max_results)
}
