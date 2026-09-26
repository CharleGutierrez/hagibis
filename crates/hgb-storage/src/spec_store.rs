use hgb_core::auto_spec::GoldenSpec;
use hgb_core::{HgbError, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub struct SpecStore;

impl SpecStore {
    fn specs_dir<P: AsRef<Path>>(workspace_root: P) -> PathBuf {
        workspace_root.as_ref().join(".hagibis").join("specs")
    }

    pub fn save_spec<P: AsRef<Path>>(workspace_root: P, spec: &GoldenSpec) -> Result<PathBuf> {
        let dir = Self::specs_dir(workspace_root);
        fs::create_dir_all(&dir)?;

        let file_path = dir.join(format!("{}.json", spec.spec_id));
        let content = serde_json::to_string_pretty(spec)
            .map_err(|e| HgbError::Storage(format!("Failed to serialize GoldenSpec: {}", e)))?;

        fs::write(&file_path, content)?;
        Ok(file_path)
    }

    pub fn load_spec<P: AsRef<Path>>(workspace_root: P, spec_id: &str) -> Result<GoldenSpec> {
        let file_path = Self::specs_dir(workspace_root).join(format!("{}.json", spec_id));
        if !file_path.exists() {
            return Err(HgbError::Storage(format!("Spec '{}' not found", spec_id)));
        }

        let content = fs::read_to_string(&file_path)?;
        let spec: GoldenSpec = serde_json::from_str(&content)
            .map_err(|e| HgbError::Storage(format!("Failed to parse GoldenSpec: {}", e)))?;

        Ok(spec)
    }

    pub fn list_specs<P: AsRef<Path>>(workspace_root: P) -> Result<Vec<GoldenSpec>> {
        let dir = Self::specs_dir(workspace_root);
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut specs = Vec::new();
        let entries = fs::read_dir(dir)?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(spec) = serde_json::from_str::<GoldenSpec>(&content) {
                        specs.push(spec);
                    }
                }
            }
        }

        specs.sort_by(|a, b| a.spec_id.cmp(&b.spec_id));
        Ok(specs)
    }

    pub fn delete_spec<P: AsRef<Path>>(workspace_root: P, spec_id: &str) -> Result<()> {
        let file_path = Self::specs_dir(workspace_root).join(format!("{}.json", spec_id));
        if file_path.exists() {
            fs::remove_file(file_path)?;
        }
        Ok(())
    }
}
