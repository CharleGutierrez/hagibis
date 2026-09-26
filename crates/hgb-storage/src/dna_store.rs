use hgb_core::drift_lock::{ArchitecturalDna, DnaPillars};
use hgb_core::{HgbError, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub struct ArchitecturalDnaStore;

impl ArchitecturalDnaStore {
    fn dna_file_path<P: AsRef<Path>>(workspace_root: P) -> PathBuf {
        workspace_root.as_ref().join(".hagibis").join("dna.json")
    }

    pub fn load_or_init<P: AsRef<Path>>(workspace_root: P) -> Result<ArchitecturalDna> {
        let file_path = Self::dna_file_path(&workspace_root);
        if file_path.exists() {
            let content = fs::read_to_string(&file_path)?;
            let dna: ArchitecturalDna = serde_json::from_str(&content)
                .map_err(|e| HgbError::Storage(format!("Failed to parse ArchitecturalDna: {}", e)))?;
            return Ok(dna);
        }

        // Initialize default architectural DNA
        let default_dna = ArchitecturalDna {
            version: 1,
            ecosystem: "rust".to_string(),
            pillars: DnaPillars::default(),
            forbidden_import_patterns: vec![
                "axios".to_string(),
                "lodash".to_string(),
                "moment".to_string(),
            ],
            forbidden_syntax_patterns: vec![
                ".unwrap()".to_string(),
                "TODO".to_string(),
                "as any".to_string(),
            ],
            created_at_rfc3339: chrono::Utc::now().to_rfc3339(),
        };

        let _ = Self::save(&workspace_root, &default_dna);
        Ok(default_dna)
    }

    pub fn save<P: AsRef<Path>>(workspace_root: P, dna: &ArchitecturalDna) -> Result<()> {
        let file_path = Self::dna_file_path(&workspace_root);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(dna)
            .map_err(|e| HgbError::Storage(format!("Failed to serialize ArchitecturalDna: {}", e)))?;

        fs::write(file_path, content)?;
        Ok(())
    }
}
