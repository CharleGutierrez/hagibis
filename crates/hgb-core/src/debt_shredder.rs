pub trait TechDebtExterminator {
    fn shred(&self) -> String;
}
pub struct AutonomousShredder;
impl TechDebtExterminator for AutonomousShredder {
    fn shred(&self) -> String {
        "Shredding tech debt autonomously!".to_string()
    }
}
