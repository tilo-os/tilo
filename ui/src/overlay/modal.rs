use super::scrim::Scrim;

#[derive(Clone, Debug)]
pub struct Modal { pub id_tag: String, pub scrim: Scrim }

impl Modal {
    pub fn new(id_tag: &str) -> Self { Self { id_tag: id_tag.into(), scrim: Scrim::dark() } }
    pub fn with_scrim(mut self, s: Scrim) -> Self { self.scrim = s; self }
}
