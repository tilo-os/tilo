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
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
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
}
