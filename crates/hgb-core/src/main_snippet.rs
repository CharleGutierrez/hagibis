        Commands::WasmFabric { action } => {
            use hgb_core::wasm_fabric::{WasmOrchestrator, FabricOrchestrator};
            let msg = FabricOrchestrator.orchestrate(&action);
            println!("🚀 {}", msg);
            Ok(())
        }
        Commands::DebtShredder { action: _ } => {
            use hgb_core::debt_shredder::{TechDebtExterminator, AutonomousShredder};
            let msg = AutonomousShredder.shred();
            println!("🧹 {}", msg);
            Ok(())
        }
        Commands::FirecrackerShield { action: _ } => {
            use hgb_core::firecracker_shield::{MicroVMSandbox, AgenticShield};
            let msg = AgenticShield.start_sandbox();
            println!("🛡️ {}", msg);
            Ok(())
        }
        Commands::NpuNative { action } => {
            use hgb_core::npu_native::{NpuOffloader, HyperLocalNpu};
            let msg = HyperLocalNpu.offload_task(&action);
            println!("⚡ {}", msg);
            Ok(())
        }
        Commands::VisionSync { action: _ } => {
            use hgb_core::vision_sync::{VisionToCode, RealTimeSync};
            let msg = RealTimeSync.sync_ui();
            println!("👁️ {}", msg);
            Ok(())
        }
