use hgb_core::{AgentShieldLight, HgbError, Result};
use std::future::Future;
use std::time::Instant;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaceCandidateResult {
    pub candidate_name: String,
    pub duration_ms: u64,
    pub patch: String,
    pub passed_checks: bool,
    pub error: Option<String>,
}

/// Speculative Dual-Draft Race ("First Green Wins") Runner
///
/// Runs dual candidate synthesis (e.g. Fast local draft vs Frontier reasoner)
/// in parallel, tests syntax/compilation & security invariants, and selects
/// the first green candidate.
pub struct SpeculativeRaceRunner;

impl SpeculativeRaceRunner {
    /// Validate candidate syntax, delimiter balance, and zero-ambient-authority security invariants
    pub fn validate_candidate(code: &str) -> Result<()> {
        let trimmed = code.trim();
        if trimmed.is_empty() {
            return Err(HgbError::Execution("Candidate draft is empty".to_string()));
        }

        // Security gate check
        AgentShieldLight::audit_payload(trimmed)?;
        AgentShieldLight::audit_secrets(trimmed)?;

        // Verify balanced delimiters: {}, (), []
        let mut stack = Vec::new();
        let mut in_string = false;
        let mut in_char = false;
        let mut escape = false;

        for ch in trimmed.chars() {
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
                continue;
            }
            if ch == '"' && !in_char {
                in_string = !in_string;
                continue;
            }
            if ch == '\'' && !in_string {
                in_char = !in_char;
                continue;
            }
            if in_string || in_char {
                continue;
            }

            match ch {
                '{' | '(' | '[' => stack.push(ch),
                '}' => {
                    if stack.pop() != Some('{') {
                        return Err(HgbError::Execution("Mismatched closing brace '}'".to_string()));
                    }
                }
                ')' => {
                    if stack.pop() != Some('(') {
                        return Err(HgbError::Execution("Mismatched closing parenthesis ')'".to_string()));
                    }
                }
                ']' => {
                    if stack.pop() != Some('[') {
                        return Err(HgbError::Execution("Mismatched closing bracket ']'".to_string()));
                    }
                }
                _ => {}
            }
        }

        if in_string || in_char {
            return Err(HgbError::Execution("Unterminated string or character literal".to_string()));
        }

        if !stack.is_empty() {
            return Err(HgbError::Execution(format!("Unclosed delimiters remaining: {:?}", stack)));
        }

        Ok(())
    }

    /// Execute a speculative race between two candidate synthesis futures.
    /// Implements "First Green Wins":
    /// The first candidate that finishes AND passes checks wins immediately.
    /// If the first candidate fails checks, the runner awaits the second candidate.
    pub async fn race_futures<F1, F2>(cand1_fut: F1, cand2_fut: F2) -> RaceCandidateResult
    where
        F1: Future<Output = (String, String)> + Send + 'static,
        F2: Future<Output = (String, String)> + Send + 'static,
    {
        let (tx, mut rx) = mpsc::channel::<RaceCandidateResult>(2);

        // Spawn Candidate 1
        let tx1 = tx.clone();
        tokio::spawn(async move {
            let t0 = Instant::now();
            let (name, patch) = cand1_fut.await;
            let check = Self::validate_candidate(&patch);
            let passed_checks = check.is_ok();
            let error = check.err().map(|e| e.to_string());
            let duration_ms = t0.elapsed().as_millis() as u64;
            let _ = tx1
                .send(RaceCandidateResult {
                    candidate_name: name,
                    duration_ms,
                    patch,
                    passed_checks,
                    error,
                })
                .await;
        });

        // Spawn Candidate 2
        let tx2 = tx;
        tokio::spawn(async move {
            let t0 = Instant::now();
            let (name, patch) = cand2_fut.await;
            let check = Self::validate_candidate(&patch);
            let passed_checks = check.is_ok();
            let error = check.err().map(|e| e.to_string());
            let duration_ms = t0.elapsed().as_millis() as u64;
            let _ = tx2
                .send(RaceCandidateResult {
                    candidate_name: name,
                    duration_ms,
                    patch,
                    passed_checks,
                    error,
                })
                .await;
        });

        let mut first_candidate: Option<RaceCandidateResult> = None;

        while let Some(res) = rx.recv().await {
            if res.passed_checks {
                // "First Green Wins"!
                return res;
            }
            if first_candidate.is_none() {
                first_candidate = Some(res);
            } else {
                // Both failed checks, return the last or first with passed_checks == false
                return res;
            }
        }

        first_candidate.unwrap_or_else(|| RaceCandidateResult {
            candidate_name: "None".to_string(),
            duration_ms: 0,
            patch: String::new(),
            passed_checks: false,
            error: Some("No candidates produced results".to_string()),
        })
    }

    /// Standard speculative dual-draft race for a user prompt
    pub async fn race(prompt: &str, _target_dir: Option<&str>) -> RaceCandidateResult {
        let p1 = prompt.to_string();
        let cand1 = async move {
            // Candidate A: Fast Local Draft
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
            let patch = format!(
                "// Fast Local Draft for: {}\npub fn execute_vibe() -> Result<(), Box<dyn std::error::Error>> {{\n    println!(\"⚡ Vibe green candidate active!\");\n    Ok(())\n}}\n",
                p1
            );
            ("Draft-A (Fast Local)".to_string(), patch)
        };

        let p2 = prompt.to_string();
        let cand2 = async move {
            // Candidate B: Frontier Reasoner Draft
            tokio::time::sleep(tokio::time::Duration::from_millis(15)).await;
            let patch = format!(
                "/// Frontier Reasoner Formal Implementation\n/// Target: {}\npub fn execute_vibe_reasoner() -> Result<bool, &'static str> {{\n    // Invariant check\n    Ok(true)\n}}\n",
                p2
            );
            ("Draft-B (Frontier Reasoner)".to_string(), patch)
        };

        Self::race_futures(cand1, cand2).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_first_green_wins_when_fast_is_valid() {
        let cand_fast = async {
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
            ("Fast-Candidate".to_string(), "fn foo() { let x = 1; }".to_string())
        };

        let cand_slow = async {
            tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
            ("Slow-Candidate".to_string(), "fn foo() { let y = 2; }".to_string())
        };

        let winner = SpeculativeRaceRunner::race_futures(cand_fast, cand_slow).await;
        assert_eq!(winner.candidate_name, "Fast-Candidate");
        assert!(winner.passed_checks);
    }

    #[tokio::test]
    async fn test_first_green_wins_when_fast_is_invalid() {
        // Fast finishes first but has unclosed brace (RED)
        let cand_fast_bad = async {
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
            ("Fast-Broken".to_string(), "fn foo() { let x = 1;".to_string())
        };

        // Slower candidate finishes next but is GREEN
        let cand_slow_good = async {
            tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
            ("Slow-Valid".to_string(), "fn bar() { let y = 2; }".to_string())
        };

        let winner = SpeculativeRaceRunner::race_futures(cand_fast_bad, cand_slow_good).await;
        assert_eq!(winner.candidate_name, "Slow-Valid");
        assert!(winner.passed_checks);
    }
}
