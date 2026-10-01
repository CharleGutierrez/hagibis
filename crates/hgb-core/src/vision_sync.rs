pub trait VisionToCode {
    fn sync_ui(&self) -> String;
}
pub struct RealTimeSync;
impl VisionToCode for RealTimeSync {
    fn sync_ui(&self) -> String {
        "Syncing multimodal vision to code".to_string()
    }
}
