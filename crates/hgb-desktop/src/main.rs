#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use hgb_desktop::commands::*;
use tauri::{generate_context, generate_handler, Builder};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let first = &args[1];
        if first == "--help" || first == "-h" {
            println!("Hagibis Next-Gen Visual IDE (hgb-desktop)");
            println!("Usage: hgb-desktop [PATH] [OPTIONS]\n");
            println!("Options:");
            println!("  -h, --help       Print help information");
            println!("  -V, --version    Print version information\n");
            println!("Arguments:");
            println!("  [PATH]           Workspace directory or file to open (default: current directory)");
            return;
        } else if first == "--version" || first == "-V" {
            println!("hgb-desktop {}", env!("CARGO_PKG_VERSION"));
            return;
        }

        let candidate = std::path::Path::new(first);
        if candidate.exists() {
            if candidate.is_dir() {
                let _ = std::env::set_current_dir(candidate);
            } else if let Some(parent) = candidate.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = std::env::set_current_dir(parent);
                }
            }
        }
    }

    Builder::default()
        .invoke_handler(generate_handler![
            get_workspace_tree,
            read_file,
            save_file,
            create_file,
            create_folder,
            delete_entry,
            rename_entry,
            compute_diff,
            commit_tab_overlay,
            ask_copilot,
            get_diagnostics,
            search_codebase,
            execute_terminal_command,
            load_project_rules,
            ask_inline_edit,
            ask_composer,
            commit_composer_plan,
            ask_tab_completion,
            resolve_context_mentions,
            get_file_git_diff,
            get_workspace_symbols,
            get_file_diagnostics,
            run_autonomous_agent,
            call_mcp_tool,
            create_checkpoint,
            rollback_checkpoint,
            get_checkpoints,
            get_available_models,
            load_workspace_session,
            save_workspace_session,
        ])
        .run(generate_context!())
        .expect("error while running Hagibis Tauri Visual IDE application");
}
