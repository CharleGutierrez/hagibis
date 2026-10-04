use std::fs;
use std::process::Command;

pub trait MicroVMSandbox {
    fn start_sandbox(&self) -> String;
}

pub struct AgenticShield;

impl MicroVMSandbox for AgenticShield {
    fn start_sandbox(&self) -> String {
        let config_path = "firecracker.toml";
        
        // Ensure some basic config exists
        if !std::path::Path::new(config_path).exists() {
            let default_config = "[machine]\nvcpu_count = 2\nmem_size_mib = 1024\n";
            let _ = fs::write(config_path, default_config);
        }
        
        let config = fs::read_to_string(config_path).unwrap_or_default();
        let vcpu = if config.contains("vcpu_count = 2") { 2 } else { 1 };
        let mem = if config.contains("mem_size_mib = 1024") { 1024 } else { 512 };

        let output = Command::new("firecracker")
            .arg("--config-file")
            .arg(config_path)
            .output();

        match output {
            Ok(out) => {
                if out.status.success() {
                    format!("Successfully spawned Firecracker MicroVM with {} vCPUs and {} MiB RAM. Output: {:?}", vcpu, mem, String::from_utf8_lossy(&out.stdout))
                } else {
                    format!("Firecracker exited with error. vCPUs: {}, RAM: {} MiB, Error: {:?}", vcpu, mem, String::from_utf8_lossy(&out.stderr))
                }
            }
            Err(e) => {
                format!("Failed to spawn Firecracker MicroVM ({} vCPUs, {} MiB RAM). Is firecracker installed? Error: {}", vcpu, mem, e)
            }
        }
    }
}
