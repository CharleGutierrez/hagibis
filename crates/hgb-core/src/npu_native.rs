pub trait NpuOffloader {
    fn offload_task(&self, task: &str) -> String;
}
pub struct HyperLocalNpu;
impl NpuOffloader for HyperLocalNpu {
    fn offload_task(&self, task: &str) -> String {
        format!("Offloading {} to NPU", task)
    }
}
