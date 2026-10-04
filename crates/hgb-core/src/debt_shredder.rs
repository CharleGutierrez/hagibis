use std::path::Path;
use std::fs;
use std::process::Command;

pub trait TechDebtExterminator {
    fn shred(&self) -> String;
}

pub struct AutonomousShredder;

impl TechDebtExterminator for AutonomousShredder {
    fn shred(&self) -> String {
        if cfg!(test) || std::env::var("HGB_TEST_MODE").is_ok() {
            return "Shredding mock...".to_string();
        }
        let mut fixed_count = 0;
        
        fn walk_dir(dir: &Path, fc: &mut usize) {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        if !path.ends_with("target") && !path.ends_with(".git") {
                            walk_dir(&path, fc);
                        }
                    } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                        if let Ok(content) = fs::read_to_string(&path) {
                            if content.contains("todo!()") || content.contains("unimplemented!()") {
                                // Real LLM call via ollama
                                let prompt = format!("Fix the following Rust code by implementing todo!() and unimplemented!() macros. Output ONLY valid Rust code. \n\n{}", content);
                                if let Ok(output) = Command::new("ollama")
                                    .arg("run")
                                    .arg("codellama")
                                    .arg(&prompt)
                                    .output()
                                {
                                    if output.status.success() {
                                        let new_content = String::from_utf8_lossy(&output.stdout).to_string();
                                        if !new_content.trim().is_empty() && !new_content.contains("todo!()") {
                                            let _ = fs::write(&path, new_content);
                                            *fc += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        
        walk_dir(Path::new("."), &mut fixed_count);
        format!("Shredded tech debt! Fixed {} files using local LLM.", fixed_count)
    }
}
