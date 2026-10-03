//! Correlates live traversal with inert layout registrations. Raw pointers are
//! compared only; even an unavailable public-map entry is never dereferenced.
use super::{HashMap, TaffyNodeId, Widget};

#[derive(Default)]
pub(crate) struct AdmittedNodeNames {
    names: HashMap<TaffyNodeId, (*mut dyn Widget, String)>,
}
impl AdmittedNodeNames {
    pub(crate) fn insert(&mut self, node: TaffyNodeId, pointer: *mut dyn Widget, name: String) {
        self.names.insert(node, (pointer, name));
    }
    pub(crate) fn remove(&mut self, node: TaffyNodeId) {
        self.names.remove(&node);
    }
    fn get(&self, node: TaffyNodeId, pointer: *mut dyn Widget) -> Option<&str> {
        let (admitted, name) = self.names.get(&node)?;
        std::ptr::eq(*admitted, pointer).then_some(name)
    }
}

struct Registration<'a> {
    id: TaffyNodeId,
    pointer: *const dyn Widget,
    name: Option<&'a str>,
}

pub(super) struct WidgetNodeMap<'a> {
    nodes: HashMap<*const (), Vec<Registration<'a>>>,
    live_names: HashMap<*const (), HashMap<String, usize>>,
}

impl<'a> WidgetNodeMap<'a> {
    pub(super) fn new<const BOUND: bool>(
        root: &dyn Widget,
        nodes: &HashMap<TaffyNodeId, *mut dyn Widget>,
        admitted: Option<&'a AdmittedNodeNames>,
    ) -> Self {
        fn collect<const BOUND: bool>(
            widget: &dyn Widget,
            names: &mut HashMap<*const (), HashMap<String, usize>>,
        ) {
            let Ok(_entry) = crate::bound_dispatch::enter_traversal::<BOUND>(widget) else {
                return;
            };
            *names
                .entry(std::ptr::from_ref(widget).cast::<()>())
                .or_default()
                .entry(widget.name().to_owned())
                .or_default() += 1;
            for child in widget.children() {
                collect::<BOUND>(child, names);
            }
        }
        let mut result = Self {
            nodes: HashMap::new(),
            live_names: HashMap::new(),
        };
        collect::<BOUND>(root, &mut result.live_names);
        for (&id, &pointer) in nodes {
            // The optional name was captured by the real live layout owner.
            // It conveys no permission, and only accompanies this exact entry.
            let name = admitted.and_then(|names| names.get(id, pointer));
            if admitted.is_some() && name.is_none() {
                continue;
            }
            result
                .nodes
                .entry(pointer.cast::<()>())
                .or_default()
                .push(Registration { id, pointer, name });
        }
        result
    }

    pub(super) fn get(&self, widget: &dyn Widget) -> Option<TaffyNodeId> {
        let pointer = std::ptr::from_ref(widget);
        let address = pointer.cast::<()>();
        let entries = self.nodes.get(&address)?;
        let names = self.live_names.get(&address)?;
        let one_live_widget = names.values().sum::<usize>() == 1;
        let unique_name = names.get(widget.name()).copied() == Some(1);
        let mut matches = entries.iter().filter(|entry| {
            std::ptr::eq(pointer, entry.pointer)
                || (unique_name && entry.name == Some(widget.name()))
                || (one_live_widget && entries.len() == 1 && entry.name.is_none())
        });
        let matched = matches.next()?;
        // Never let HashMap order choose between multiple layout registrations.
        matches.next().is_none().then_some(matched.id)
    }
}
