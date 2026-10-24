pub mod canvas;
pub mod client;
pub mod repl;
pub mod menu;
pub mod desktop_ide;
pub mod init_vibe;
pub mod setup_ide;

pub use canvas::{ChatCanvas, ToolCallCard, ToolCardStatus};
pub use client::HgbClient;
pub use repl::HagibisRepl;
pub use menu::InteractiveCommandCatalog;
pub use desktop_ide::{DesktopIdeLauncher, DesktopLaunchConfig, DesktopLaunchReport};
pub use init_vibe::{VibeOnboardingEngine, VibeOnboardingReport};
pub use setup_ide::{run_setup, IdeOnboardingEngine, IdeSetupResult, SetupArgs};
