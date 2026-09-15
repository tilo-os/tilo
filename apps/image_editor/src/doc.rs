use crate::model::Project;
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
pub struct ProjectDoc;
impl ProjectDoc {
    pub fn read(doc: &AutoCommit) -> Project {
        doc.get(&ROOT, "image-editor.project")
            .ok()
            .flatten()
            .and_then(|(v, _)| v.into_scalar().ok())
            .and_then(|v| v.to_str().map(str::to_owned))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(Project::blank)
    }
    pub fn write(doc: &mut AutoCommit, p: &Project) {
        if let Ok(s) = serde_json::to_string(p) {
            let _ = doc.put(&ROOT, "image-editor.project", s);
        }
    }
    pub fn seed(doc: &mut AutoCommit) {
        if doc
            .get(&ROOT, "image-editor.project")
            .ok()
            .flatten()
            .is_none()
        {
            Self::write(doc, &Project::blank());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Operation;
    #[test]
    fn graph_round_trips_through_crdt() {
        let mut d = AutoCommit::new();
        ProjectDoc::seed(&mut d);
        let mut p = ProjectDoc::read(&d);
        p.insert_before_output("tone", "Contrast", Operation::Contrast { amount: 0.2 });
        ProjectDoc::write(&mut d, &p);
        assert_eq!(ProjectDoc::read(&d), p);
    }
}
