pub trait NpuOffloader {
    fn offload_task(&self, task: &str) -> String;
}
pub struct HyperLocalNpu;
impl NpuOffloader for HyperLocalNpu {
    fn offload_task(&self, task: &str) -> String {
        use sha2::{Sha256, Digest};
        let mut hash = vec![0u8; 32];
        for i in 0..10_000 {
            let mut hasher = Sha256::new();
            hasher.update(&hash);
            hasher.update(task.as_bytes());
            hasher.update(i.to_string().as_bytes());
            hash = hasher.finalize().to_vec();
        }
        format!("Offloading {} to NPU. Simulated compute done.", task)
    }
}
