#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use hgb_desktop::commands::*;
use tauri::{generate_context, generate_handler, Builder};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let candidate = std::path::Path::new(&args[1]);
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
        ])
        .run(generate_context!())
        .expect("error while running Hagibis Tauri Visual IDE application");
}
