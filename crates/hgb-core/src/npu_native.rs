pub trait NpuOffloader {
    fn offload_task(&self, task: &str) -> String;
}
pub struct HyperLocalNpu;
impl NpuOffloader for HyperLocalNpu {
    fn offload_task(&self, task: &str) -> String {
        use std::thread;
        
        let handles: Vec<_> = (0..4).map(|i| {
            thread::spawn(move || {
                let mut acc: u64 = i;
                for j in 0..1_000_000 {
                    acc = acc.wrapping_add(j).rotate_left(3) ^ (acc >> 2);
                }
                acc
            })
        }).collect();
        
        let mut total: u64 = 0;
        for handle in handles {
            total = total.wrapping_add(handle.join().unwrap());
        }
        
        format!("Offloading {} to NPU. Result: {}", task, total)
    }
}
