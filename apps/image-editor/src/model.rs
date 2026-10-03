use serde::{Deserialize, Serialize};
use ui::graph_editor::{Edge, Graph, GraphNode, GraphPoint, Port};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Operation {
    Source {
        path: String,
    },
    Exposure {
        stops: f32,
    },
    Contrast {
        amount: f32,
    },
    Saturation {
        amount: f32,
    },
    Grayscale,
    Rotate {
        quarter_turns: u8,
    },
    Crop {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    Output,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub graph: Graph,
    pub operations: std::collections::BTreeMap<String, Operation>,
    pub output: String,
}
fn port(id: &str) -> Port {
    Port {
        id: id.into(),
        data_type: "image/rgba8".into(),
        label: "Image".into(),
    }
}
fn node(id: &str, title: &str, x: f64, inputs: bool) -> GraphNode {
    GraphNode {
        id: id.into(),
        title: title.into(),
        position: GraphPoint { x, y: 120.0 },
        inputs: if inputs { vec![port("image")] } else { vec![] },
        outputs: vec![port("image")],
        enabled: true,
    }
}
impl Project {
    pub fn blank() -> Self {
        let mut graph = Graph::default();
        graph.insert(node("source", "Open an image", 40.0, false));
        graph.insert(node("output", "Export", 600.0, true));
        let mut operations = std::collections::BTreeMap::new();
        operations.insert(
            "source".into(),
            Operation::Source {
                path: String::new(),
            },
        );
        operations.insert("output".into(), Operation::Output);
        let _ = graph.connect(Edge {
            from_node: "source".into(),
            from_port: "image".into(),
            to_node: "output".into(),
            to_port: "image".into(),
        });
        Self {
            name: "Untitled image".into(),
            graph,
            operations,
            output: "output".into(),
        }
    }
    pub fn insert_before_output(&mut self, id: &str, title: &str, op: Operation) -> bool {
        if self.graph.nodes.contains_key(id) {
            return false;
        }
        let incoming = self.graph.disconnect_input(&self.output, "image");
        let x = self
            .graph
            .nodes
            .get(&self.output)
            .map_or(400.0, |n| n.position.x - 180.0);
        self.graph.insert(node(id, title, x, true));
        self.operations.insert(id.into(), op);
        if let Some(old) = incoming {
            let _ = self.graph.connect(Edge {
                to_node: id.into(),
                to_port: "image".into(),
                ..old
            });
        }
        let connected = self
            .graph
            .connect(Edge {
                from_node: id.into(),
                from_port: "image".into(),
                to_node: self.output.clone(),
                to_port: "image".into(),
            })
            .is_ok();
        if connected && let Ok(order) = self.graph.evaluation_order() {
            for (index, node_id) in order.into_iter().enumerate() {
                if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                    node.position = GraphPoint {
                        x: 40.0 + f64::from(u32::try_from(index).unwrap_or(u32::MAX)) * 200.0,
                        y: 120.0,
                    };
                }
            }
        }
        connected
    }

    /// Remove an image-processing step without breaking the work after it.
    ///
    /// Each adjustment has one image input and may feed more than one later
    /// step in the advanced graph. Splice its upstream image into every
    /// downstream input before removing it. Work on a side branch therefore
    /// stays live just like the familiar linear adjustment stack does.
    pub fn remove_adjustment(&mut self, id: &str) -> bool {
        if id == "source" || id == self.output || !self.operations.contains_key(id) {
            return false;
        }

        let incoming = self
            .graph
            .edges
            .iter()
            .find(|edge| edge.to_node == id && edge.to_port == "image")
            .cloned();
        let outgoing = self
            .graph
            .edges
            .iter()
            .filter(|edge| edge.from_node == id && edge.from_port == "image")
            .cloned()
            .collect::<Vec<_>>();

        // Perform the splice on a copy so an unexpected imported graph shape
        // can never turn Delete into a partially destructive operation.
        let mut graph = self.graph.clone();
        if graph.remove(id).is_none() {
            return false;
        }
        if let Some(upstream) = incoming {
            for downstream in outgoing {
                if graph
                    .connect(Edge {
                        from_node: upstream.from_node.clone(),
                        from_port: upstream.from_port.clone(),
                        to_node: downstream.to_node,
                        to_port: downstream.to_port,
                    })
                    .is_err()
                {
                    return false;
                }
            }
        }

        self.graph = graph;
        self.operations.remove(id);
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Inserting and removing adjustments rewires the graph without flattening,
    /// splices every branch, and refuses structural nodes.
    #[test]
    fn adjustment_insert_and_remove_rewire_the_graph() {
        inserting_an_early_step_rewires_without_flattening();
        removing_a_middle_adjustment_preserves_every_later_step();
        removing_an_adjustment_splices_all_of_its_branches();
        structural_nodes_cannot_be_removed_as_adjustments();
    }

    fn inserting_an_early_step_rewires_without_flattening() {
        let mut p = Project::blank();
        assert!(p.insert_before_output("grade", "Exposure", Operation::Exposure { stops: 1.0 }));
        assert!(p.insert_before_output(
            "crop",
            "Crop",
            Operation::Crop {
                x: 0.0,
                y: 0.0,
                width: 0.8,
                height: 0.8
            }
        ));
        assert_eq!(
            p.graph.evaluation_order().unwrap(),
            ["source", "grade", "crop", "output"]
        );
        assert!(matches!(p.operations["grade"], Operation::Exposure { .. }));
    }

    fn removing_a_middle_adjustment_preserves_every_later_step() {
        let mut p = Project::blank();
        assert!(p.insert_before_output("grade", "Exposure", Operation::Exposure { stops: 1.0 }));
        assert!(p.insert_before_output(
            "crop",
            "Crop",
            Operation::Crop {
                x: 0.0,
                y: 0.0,
                width: 0.8,
                height: 0.8
            }
        ));

        assert!(p.remove_adjustment("grade"));
        assert_eq!(
            p.graph.evaluation_order().unwrap(),
            ["source", "crop", "output"]
        );
        assert!(p.graph.edges.iter().any(|edge| {
            edge.from_node == "source" && edge.to_node == "crop" && edge.to_port == "image"
        }));
        assert!(!p.operations.contains_key("grade"));
    }

    fn removing_an_adjustment_splices_all_of_its_branches() {
        let mut p = Project::blank();
        assert!(p.insert_before_output("grade", "Exposure", Operation::Exposure { stops: 1.0 }));
        p.graph.insert(node("preview", "Preview", 440.0, true));
        p.operations.insert("preview".into(), Operation::Output);
        p.graph
            .connect(Edge {
                from_node: "grade".into(),
                from_port: "image".into(),
                to_node: "preview".into(),
                to_port: "image".into(),
            })
            .unwrap();

        assert!(p.remove_adjustment("grade"));
        assert!(
            p.graph
                .edges
                .iter()
                .any(|edge| { edge.from_node == "source" && edge.to_node == "output" })
        );
        assert!(
            p.graph
                .edges
                .iter()
                .any(|edge| { edge.from_node == "source" && edge.to_node == "preview" })
        );
    }

    fn structural_nodes_cannot_be_removed_as_adjustments() {
        let mut p = Project::blank();
        let before = p.clone();
        assert!(!p.remove_adjustment("source"));
        assert!(!p.remove_adjustment("output"));
        assert_eq!(p, before);
    }
}
