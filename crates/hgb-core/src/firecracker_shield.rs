pub trait MicroVMSandbox {
    fn start_sandbox(&self) -> String;
}
pub struct AgenticShield;
impl MicroVMSandbox for AgenticShield {
    fn start_sandbox(&self) -> String {
        "Starting Firecracker MicroVM shield".to_string()
    }
}
