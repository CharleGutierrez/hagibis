pub mod canvas;
pub mod client;
pub mod repl;
pub mod menu;
pub mod desktop_ide;
pub mod init_vibe;
pub mod setup_ide;
pub mod pgo_benchmark;

pub use canvas::{ChatCanvas, ToolCallCard, ToolCardStatus};
pub use client::HgbClient;
pub use repl::HagibisRepl;
pub use menu::InteractiveCommandCatalog;
pub use desktop_ide::{DesktopIdeLauncher, DesktopLaunchConfig, DesktopLaunchReport};
pub use init_vibe::{AppScaffolder, FrameworkType, ScaffoldReport, VibeOnboardingEngine, VibeOnboardingReport};
pub use setup_ide::{install_ide_mcp, run_setup, IdeOnboardingEngine, IdeSetupResult, IdeTarget, InstalledIdeInfo, SetupArgs};
pub use pgo_benchmark::run_pgo_benchmark;
