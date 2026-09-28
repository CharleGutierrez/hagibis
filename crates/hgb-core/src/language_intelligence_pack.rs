use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SupportedFramework {
    RubyOnRails,
    PythonFastAPI,
    TypeScriptNextJs,
    GoFiber,
    JavaSpringBoot,
    GenericRust,
    Unknown(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguagePackInfo {
    pub framework: SupportedFramework,
    pub primary_language: String,
    pub recommended_linter: String,
    pub test_runner_command: String,
    pub conventions: Vec<String>,
    pub boilerplate_templates: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameworkDiagnosisReport {
    pub detected_framework: SupportedFramework,
    pub language_info: LanguagePackInfo,
    pub health_score: f32, // 0.0 to 1.0
    pub detected_config_files: Vec<String>,
    pub missing_recommended_files: Vec<String>,
}

pub struct LanguageIntelligencePack;

impl LanguageIntelligencePack {
    pub fn new() -> Self {
        Self
    }

    /// Detects ecosystem and returns targeted language intelligence pack
    pub fn inspect_workspace(workspace_root: &Path) -> FrameworkDiagnosisReport {
        let (framework, primary_lang, linter, test_cmd, conventions, configs, missing) =
            if workspace_root.join("config/routes.rb").exists() || workspace_root.join("Gemfile").exists() {
                (
                    SupportedFramework::RubyOnRails,
                    "Ruby".to_string(),
                    "rubocop -A".to_string(),
                    "bundle exec rspec".to_string(),
                    vec![
                        "Convention over Configuration".to_string(),
                        "Zero-downtime concurrent migrations".to_string(),
                        "Eager load associations to eliminate N+1 queries".to_string(),
                        "Strong parameters in controllers".to_string(),
                    ],
                    vec!["Gemfile".to_string(), "config/routes.rb".to_string()],
                    if !workspace_root.join(".rubocop.yml").exists() { vec![".rubocop.yml".to_string()] } else { vec![] },
                )
            } else if workspace_root.join("pyproject.toml").exists() || workspace_root.join("requirements.txt").exists() {
                (
                    SupportedFramework::PythonFastAPI,
                    "Python".to_string(),
                    "ruff check --fix".to_string(),
                    "pytest -v".to_string(),
                    vec![
                        "Pydantic schema validation".to_string(),
                        "Type annotations on all route parameters".to_string(),
                        "Async def for I/O bound endpoints".to_string(),
                    ],
                    vec!["pyproject.toml".to_string()],
                    if !workspace_root.join("ruff.toml").exists() { vec!["ruff.toml".to_string()] } else { vec![] },
                )
            } else if workspace_root.join("package.json").exists() {
                (
                    SupportedFramework::TypeScriptNextJs,
                    "TypeScript".to_string(),
                    "eslint --fix".to_string(),
                    "npm test".to_string(),
                    vec![
                        "React Server Components by default".to_string(),
                        "Server Actions for data mutations".to_string(),
                        "Tailwind CSS utility styling".to_string(),
                    ],
                    vec!["package.json".to_string()],
                    if !workspace_root.join("tsconfig.json").exists() { vec!["tsconfig.json".to_string()] } else { vec![] },
                )
            } else if workspace_root.join("go.mod").exists() {
                (
                    SupportedFramework::GoFiber,
                    "Go".to_string(),
                    "golangci-lint run --fix".to_string(),
                    "go test ./...".to_string(),
                    vec![
                        "Zero-allocation structs".to_string(),
                        "Explicit error checking (if err != nil)".to_string(),
                        "Context propagation on goroutines".to_string(),
                    ],
                    vec!["go.mod".to_string()],
                    vec![],
                )
            } else if workspace_root.join("pom.xml").exists() || workspace_root.join("build.gradle").exists() {
                (
                    SupportedFramework::JavaSpringBoot,
                    "Java".to_string(),
                    "mvn checkstyle:check".to_string(),
                    "mvn test".to_string(),
                    vec![
                        "Spring Dependency Injection".to_string(),
                        "Spring Data JPA repository patterns".to_string(),
                        "Lombok model annotations".to_string(),
                    ],
                    vec!["pom.xml".to_string()],
                    vec![],
                )
            } else {
                (
                    SupportedFramework::GenericRust,
                    "Rust".to_string(),
                    "cargo clippy --fix --allow-dirty".to_string(),
                    "cargo test".to_string(),
                    vec![
                        "Zero panics on runtime paths".to_string(),
                        "Bincode length-delimited serialization".to_string(),
                        "Saturating arithmetic and defensive bounds".to_string(),
                    ],
                    vec!["Cargo.toml".to_string()],
                    vec![],
                )
            };

        let health_score = if missing.is_empty() { 1.0 } else { 0.85 };

        let language_info = LanguagePackInfo {
            framework: framework.clone(),
            primary_language: primary_lang,
            recommended_linter: linter,
            test_runner_command: test_cmd,
            conventions,
            boilerplate_templates: vec!["scaffold".to_string(), "dockerfile".to_string(), "ci-workflow".to_string()],
        };

        FrameworkDiagnosisReport {
            detected_framework: framework,
            language_info,
            health_score,
            detected_config_files: configs,
            missing_recommended_files: missing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inspect_workspace_rails() {
        let tmp = std::env::temp_dir().join("hgb_lang_rails_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("config")).unwrap();
        std::fs::write(tmp.join("config/routes.rb"), "Rails.application.routes.draw do\nend").unwrap();
        std::fs::write(tmp.join("Gemfile"), "gem 'rails'").unwrap();

        let rep = LanguageIntelligencePack::inspect_workspace(&tmp);
        assert_eq!(rep.detected_framework, SupportedFramework::RubyOnRails);
        assert_eq!(rep.language_info.primary_language, "Ruby");
        assert!(rep.language_info.recommended_linter.contains("rubocop"));
        assert!(rep.language_info.test_runner_command.contains("rspec"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
