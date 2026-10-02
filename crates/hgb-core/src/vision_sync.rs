use std::process::Command;

pub trait VisionToCode {
    fn sync_ui(&self) -> String;
}
pub struct RealTimeSync;
impl VisionToCode for RealTimeSync {
    fn sync_ui(&self) -> String {
        let output = if cfg!(target_os = "macos") {
            Command::new("pbpaste").output()
        } else {
            Command::new("xclip").arg("-selection").arg("clipboard").arg("-o").output()
                .or_else(|_| Command::new("xclip").arg("-o").output())
        };

        match output {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).to_string()
            }
            _ => "Syncing multimodal vision to code".to_string()
        }
    }
}
