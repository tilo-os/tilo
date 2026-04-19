#[derive(Clone, Debug)]
pub struct FocusTrap { members: Vec<u64> }

impl FocusTrap {
    pub fn new(members: Vec<u64>) -> Self { Self { members } }
    pub fn next_from(&self, id: u64) -> u64 {
        let idx = self.members.iter().position(|&x| x == id).unwrap_or(0);
        self.members[(idx + 1) % self.members.len()]
    }
    pub fn prev_from(&self, id: u64) -> u64 {
        let idx = self.members.iter().position(|&x| x == id).unwrap_or(0);
        self.members[(idx + self.members.len() - 1) % self.members.len()]
    }
    pub fn contains(&self, id: u64) -> bool { self.members.contains(&id) }
}
