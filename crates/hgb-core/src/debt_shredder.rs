use std::fs;

pub trait TechDebtExterminator {
    fn shred(&self) -> String;
}
pub struct AutonomousShredder;
impl TechDebtExterminator for AutonomousShredder {
    fn shred(&self) -> String {
        let mut debt_score = 0;
        
        let mut count_unwraps = |path: &std::path::Path| {
            if let Ok(content) = fs::read_to_string(path) {
                debt_score += content.matches("unwrap()").count();
            }
        };

        if let Ok(entries) = fs::read_dir(".") {
            for entry in entries.flatten() {
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_file() {
                        if entry.path().extension().and_then(|e| e.to_str()) == Some("rs") {
                            count_unwraps(&entry.path());
                        }
                    }
                }
            }
        }
        
        format!("Shredding tech debt autonomously! Real Tech Debt Score: {}", debt_score)
    }
}
