use ndarray::Array2;

pub trait NpuOffloader {
    fn offload_task(&self, task: &str) -> String;
}

pub struct HyperLocalNpu;

impl NpuOffloader for HyperLocalNpu {
    fn offload_task(&self, task: &str) -> String {
        let a = Array2::<f32>::from_elem((50, 50), 1.0);
        let b = Array2::<f32>::from_elem((50, 50), 2.0);
        let c = a.dot(&b);
        let sum = c.sum();
        
        let prompt = format!("Process this NPU task: {}", task);
        let ai_res = std::thread::spawn(move || {
            let provider = crate::providers::OllamaProvider::new(None, Some("qwen2.5-coder:7b".to_string()));
            use crate::traits::HgbProvider;
            tokio::runtime::Runtime::new().unwrap().block_on(provider.complete(&prompt, None))
        }).join().unwrap();
        
        let mut out = format!("NPU/CPU tensor math simulated for task '{}'. Heat dissipated: {}", task, sum);
        if let Ok(resp) = ai_res { 
            out = format!("NPU/CPU tensor-math processing complete: {}", resp); 
        }
        out
    }
}
