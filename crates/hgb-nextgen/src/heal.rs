use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use regex::Regex;
use serde::{Deserialize, Serialize};
use hgb_core::{AgyCrud, CommandOptions, HgbProvider, ReplaceOptions, Result, ViewFileOptions};
use crate::checkpoint::SwarmCheckpointManager;

/// A structured compiler or test diagnostic message
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompilerDiagnostic {
    /// Relative or absolute path to the faulty source file
    pub file_path: PathBuf,
    /// 1-indexed primary line number
    pub line_number: usize,
    /// 1-indexed column number
    pub column_number: usize,
    /// Error code (e.g. "E0308", "E0425", "TS2322")
    pub error_code: Option<String>,
    /// Primary error description
    pub message: String,
    /// Formatted compiler source span or hint
    pub rendered_snippet: Option<String>,
}

/// A recorded attempt at fixing an error
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealAttempt {
    pub attempt_number: usize,
    pub diagnostic: CompilerDiagnostic,
    pub target_file: PathBuf,
    pub original_snippet: String,
    pub replacement_snippet: String,
    pub success: bool,
    pub duration_ms: u64,
}

/// Complete report returned by the Self-Healing Engine
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealReport {
    pub check_command: String,
    pub initial_error_count: usize,
    pub final_error_count: usize,
    pub attempts: Vec<HealAttempt>,
    pub fully_healed: bool,
    pub duration_ms: u64,
    pub checkpoint_ids: Vec<String>,
}

pub struct HealEngine {
    provider: Arc<dyn HgbProvider>,
    workspace_root: PathBuf,
    max_iterations: usize,
}

impl HealEngine {
    pub fn new(provider: Arc<dyn HgbProvider>, workspace_root: PathBuf) -> Self {
        Self {
            provider,
            workspace_root,
            max_iterations: 5,
        }
    }

    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Automatically infer the primary check command for the workspace
    pub fn detect_check_command(&self) -> String {
        if self.workspace_root.join("Cargo.toml").exists() {
            "cargo check --workspace".to_string()
        } else if self.workspace_root.join("package.json").exists() {
            "npm test".to_string()
        } else if self.workspace_root.join("go.mod").exists() {
            "go test ./...".to_string()
        } else if self.workspace_root.join("pyproject.toml").exists() || self.workspace_root.join("pytest.ini").exists() {
            "pytest".to_string()
        } else {
            "cargo check".to_string()
        }
    }

    /// Parse stderr or combined output to extract individual compiler diagnostics
    pub fn parse_diagnostics(raw_output: &str) -> Vec<CompilerDiagnostic> {
        let mut diagnostics = Vec::new();

        // 1. Rustc standard diagnostic pattern: `error[E0308]: mismatched types\n  --> src/main.rs:42:15`
        let rustc_re = Regex::new(r"(?m)error(?:\[([A-Z0-9]+)\])?: (.*?)\n\s+-->\s+(.*?):(\d+):(\d+)").unwrap();
        for cap in rustc_re.captures_iter(raw_output) {
            let error_code = cap.get(1).map(|m| m.as_str().to_string());
            let message = cap.get(2).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
            let file_str = cap.get(3).map(|m| m.as_str().to_string()).unwrap_or_default();
            let line_number = cap.get(4).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
            let column_number = cap.get(5).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);

            diagnostics.push(CompilerDiagnostic {
                file_path: PathBuf::from(file_str),
                line_number,
                column_number,
                error_code,
                message,
                rendered_snippet: None,
            });
        }

        // 2. TypeScript / Generic compiler pattern: `src/index.ts:45:10 - error TS2322: Type 'string' is not assignable...`
        if diagnostics.is_empty() {
            let ts_re = Regex::new(r"(?m)(.*?):(\d+):(\d+)\s+-\s+error\s+([A-Z0-9]+)?:\s+(.*)").unwrap();
            for cap in ts_re.captures_iter(raw_output) {
                let file_str = cap.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                let line_number = cap.get(2).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
                let column_number = cap.get(3).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
                let error_code = cap.get(4).map(|m| m.as_str().to_string());
                let message = cap.get(5).map(|m| m.as_str().trim().to_string()).unwrap_or_default();

                diagnostics.push(CompilerDiagnostic {
                    file_path: PathBuf::from(file_str),
                    line_number,
                    column_number,
                    error_code,
                    message,
                    rendered_snippet: None,
                });
            }
        }

        diagnostics
    }

    /// Execute the autonomous self-healing iteration loop
    pub async fn heal(
        &self,
        command_override: Option<&str>,
        ckpt_mgr: &mut SwarmCheckpointManager,
    ) -> Result<HealReport> {
        let check_cmd = command_override.map(|s| s.to_string()).unwrap_or_else(|| self.detect_check_command());
        let start_time = Instant::now();

        // 1. Initial verification run
        let initial_res = AgyCrud::run_command(
            &check_cmd,
            Some(&self.workspace_root),
            CommandOptions::default(),
        ).await?;

        if initial_res.exit_code == 0 {
            return Ok(HealReport {
                check_command: check_cmd,
                initial_error_count: 0,
                final_error_count: 0,
                attempts: Vec::new(),
                fully_healed: true,
                duration_ms: start_time.elapsed().as_millis() as u64,
                checkpoint_ids: Vec::new(),
            });
        }

        let mut diagnostics = Self::parse_diagnostics(&initial_res.combined_output);
        let initial_error_count = diagnostics.len().max(1);
        let mut attempts = Vec::new();
        let mut checkpoint_ids = Vec::new();
        let mut iteration = 0;

        while iteration < self.max_iterations && !diagnostics.is_empty() {
            iteration += 1;
            let diag = &diagnostics[0];

            let target_abs = if diag.file_path.is_absolute() {
                diag.file_path.clone()
            } else {
                self.workspace_root.join(&diag.file_path)
            };

            if !target_abs.exists() {
                break;
            }

            // 2. Fetch context around error span
            let start_line = diag.line_number.saturating_sub(15).max(1);
            let end_line = diag.line_number + 15;
            let context_res = AgyCrud::view_file(&target_abs, ViewFileOptions {
                start_line: Some(start_line),
                end_line: Some(end_line),
                content_offset: None,
                max_lines: Some(50),
                line_numbers: true,
            })?;

            // 3. Create time-travel safety checkpoint before attempting edit
            ckpt_mgr.snapshot_file(&target_abs)?;
            let ckpt = ckpt_mgr.create_checkpoint(&format!("heal-iter-{}", iteration), HashMap::new(), HashMap::new());
            checkpoint_ids.push(ckpt.checkpoint_id.clone());

            // 4. Query model for surgical fix
            let repair_prompt = format!(
                "You are an automated compiler self-healing repair engine.\n\
                The command `{}` failed with the following diagnostic:\n\
                Error: {}\nCode: {:?}\nFile: {}:{}\n\n\
                Source snippet around error line:\n```\n{}\n```\n\n\
                Return ONLY a JSON block specifying exact surgical search-and-replace strings:\n\
                ```json\n\
                {{\n\
                  \"target\": \"<exact original code to replace>\",\n\
                  \"replacement\": \"<corrected code>\"\n\
                }}\n\
                ```",
                check_cmd, diag.message, diag.error_code, target_abs.display(), diag.line_number, context_res.content
            );

            let model_resp = self.provider.complete(&repair_prompt, None).await?;

            // 5. Parse and apply surgical replacement
            if let Some((target, replacement)) = Self::extract_repair_json(&model_resp) {
                let apply_res = AgyCrud::replace_file_content(
                    &target_abs,
                    &target,
                    &replacement,
                    ReplaceOptions {
                        start_line: Some(start_line),
                        end_line: Some(end_line),
                        allow_multiple: false,
                        create_backup: false,
                        instruction: Some("Compiler self-healing automated repair".to_string()),
                        description: Some(diag.message.clone()),
                        target_lint_error_ids: diag.error_code.clone().into_iter().collect(),
                    },
                );

                match apply_res {
                    Ok(_) => {
                        // 6. Re-run check command to verify
                        let check_res = AgyCrud::run_command(
                            &check_cmd,
                            Some(&self.workspace_root),
                            CommandOptions::default(),
                        ).await?;

                        let new_diags = Self::parse_diagnostics(&check_res.combined_output);

                        if check_res.exit_code == 0 {
                            attempts.push(HealAttempt {
                                attempt_number: iteration,
                                diagnostic: diag.clone(),
                                target_file: target_abs,
                                original_snippet: target,
                                replacement_snippet: replacement,
                                success: true,
                                duration_ms: start_time.elapsed().as_millis() as u64,
                            });
                            return Ok(HealReport {
                                check_command: check_cmd,
                                initial_error_count,
                                final_error_count: 0,
                                attempts,
                                fully_healed: true,
                                duration_ms: start_time.elapsed().as_millis() as u64,
                                checkpoint_ids,
                            });
                        } else if !new_diags.is_empty() && new_diags.len() > diagnostics.len() {
                            // Regression occurred: rollback to checkpoint!
                            let _ = ckpt_mgr.restore_files_from_checkpoint(&ckpt.checkpoint_id);
                            attempts.push(HealAttempt {
                                attempt_number: iteration,
                                diagnostic: diag.clone(),
                                target_file: target_abs,
                                original_snippet: target,
                                replacement_snippet: replacement,
                                success: false,
                                duration_ms: start_time.elapsed().as_millis() as u64,
                            });
                        } else {
                            // Progress made (errors decreased or shifted)
                            diagnostics = new_diags;
                        }
                    }
                    Err(_) => {
                        // Edit failed to apply cleanly
                        let _ = ckpt_mgr.restore_files_from_checkpoint(&ckpt.checkpoint_id);
                    }
                }
            } else {
                break;
            }
        }

        let final_count = diagnostics.len();
        Ok(HealReport {
            check_command: check_cmd,
            initial_error_count,
            final_error_count: final_count,
            attempts,
            fully_healed: final_count == 0,
            duration_ms: start_time.elapsed().as_millis() as u64,
            checkpoint_ids,
        })
    }

    pub fn extract_repair_json(text: &str) -> Option<(String, String)> {
        #[derive(Deserialize)]
        struct Patch {
            target: String,
            replacement: String,
        }

        let marker = "```json";
        if let Some(start) = text.find(marker) {
            let rest = &text[start + marker.len()..];
            if let Some(end) = rest.find("```") {
                let json_str = rest[..end].trim();
                if let Ok(p) = serde_json::from_str::<Patch>(json_str) {
                    return Some((p.target, p.replacement));
                }
            }
        }

        // Direct JSON string parsing fallback
        if let Ok(p) = serde_json::from_str::<Patch>(text.trim()) {
            return Some((p.target, p.replacement));
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rustc_diagnostics() {
        let rustc_out = r#"
error[E0308]: mismatched types
  --> src/main.rs:42:15
   |
42 |     let x: u32 = "hello";
   |            ---   ^^^^^^^ expected `u32`, found `&str`
   |            |
   |            expected due to this
"#;

        let diags = HealEngine::parse_diagnostics(rustc_out);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file_path, PathBuf::from("src/main.rs"));
        assert_eq!(diags[0].line_number, 42);
        assert_eq!(diags[0].column_number, 15);
        assert_eq!(diags[0].error_code.as_deref(), Some("E0308"));
        assert!(diags[0].message.contains("mismatched types"));
    }

    #[test]
    fn test_extract_repair_json() {
        let text = r#"Here is the fix:
```json
{
  "target": "let x: u32 = \"hello\";",
  "replacement": "let x: &str = \"hello\";"
}
```
"#;
        let res = HealEngine::extract_repair_json(text);
        assert!(res.is_some());
        let (target, repl) = res.unwrap();
        assert_eq!(target, "let x: u32 = \"hello\";");
        assert_eq!(repl, "let x: &str = \"hello\";");
    }
}
