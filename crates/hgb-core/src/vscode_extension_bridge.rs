use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VsCodeBridgeConfig {
    pub extension_name: String,
    pub socket_path: String,
    pub enable_inline_diffs: bool,
    pub enable_ghost_suggest: bool,
    pub enable_statusbar_widget: bool,
}

impl Default for VsCodeBridgeConfig {
    fn default() -> Self {
        Self {
            extension_name: "hagibis-vibe-bridge".to_string(),
            socket_path: "/tmp/hgbd.sock".to_string(),
            enable_inline_diffs: true,
            enable_ghost_suggest: true,
            enable_statusbar_widget: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VsCodeExtensionScaffoldReport {
    pub manifest_json: String,
    pub extension_ts_code: String,
    pub socket_path: String,
    pub registered_commands: Vec<String>,
    pub install_instructions: String,
}

pub struct VsCodeExtensionBridge;

impl VsCodeExtensionBridge {
    pub fn new() -> Self {
        Self
    }

    /// Generates complete VS Code / Cursor extension package and TypeScript IPC bridge
    pub fn scaffold_extension(config: Option<VsCodeBridgeConfig>) -> VsCodeExtensionScaffoldReport {
        let cfg = config.unwrap_or_default();

        let commands = vec![
            "hgb.explainSelection".to_string(),
            "hgb.autopilotTicket".to_string(),
            "hgb.openCockpit".to_string(),
            "hgb.railsAudit".to_string(),
            "hgb.budgetStatus".to_string(),
            "hgb.timeWarpRewind".to_string(),
        ];

        let manifest_json = format!(
            r#"{{
  "name": "{}",
  "displayName": "Hagibis (hgb) Vibe Coding Companion",
  "description": "Ultra-low-latency IPC bridge to resident Hagibis Daemon (hgbd)",
  "version": "1.0.0",
  "publisher": "hagibis",
  "engines": {{
    "vscode": "^1.90.0"
  }},
  "categories": ["Programming Languages", "Linters", "AI"],
  "activationEvents": ["onStartupFinished"],
  "main": "./dist/extension.js",
  "contributes": {{
    "commands": [
      {{ "command": "hgb.explainSelection", "title": "Hagibis: Explain Decision (ADR)" }},
      {{ "command": "hgb.autopilotTicket", "title": "Hagibis: Autopilot Ticket-to-PR" }},
      {{ "command": "hgb.openCockpit", "title": "Hagibis: Launch TUI Cockpit" }},
      {{ "command": "hgb.railsAudit", "title": "Hagibis: Rails Zero-Downtime Audit" }},
      {{ "command": "hgb.budgetStatus", "title": "Hagibis: LLM FinOps Budget Status" }},
      {{ "command": "hgb.timeWarpRewind", "title": "Hagibis: ChronoWarp Rewind" }}
    ],
    "keybindings": [
      {{ "command": "hgb.explainSelection", "key": "ctrl+alt+e", "mac": "cmd+alt+e" }},
      {{ "command": "hgb.autopilotTicket", "key": "ctrl+alt+a", "mac": "cmd+alt+a" }}
    ]
  }}
}}
"#,
            cfg.extension_name
        );

        let extension_ts_code = format!(
            r#"import * as vscode from 'vscode';
import * as net from 'net';

const SOCKET_PATH = '{}';

function queryHgbd(requestPayload: object): Promise<any> {{
  return new Promise((resolve, reject) => {{
    const client = net.createConnection(SOCKET_PATH, () => {{
      const json = JSON.stringify(requestPayload);
      client.write(json);
    }});
    let buffer = '';
    client.on('data', (data) => {{
      buffer += data.toString();
    }});
    client.on('end', () => {{
      try {{
        resolve(JSON.parse(buffer));
      }} catch (err) {{
        resolve({{ status: 'raw', data: buffer }});
      }}
    }});
    client.on('error', (err) => {{
      reject(err);
    }});
  }});
}}

export function activate(context: vscode.ExtensionContext) {{
  console.log('⚡ Hagibis Vibe Bridge active. Connected to ' + SOCKET_PATH);

  context.subscriptions.push(
    vscode.commands.registerCommand('hgb.explainSelection', async () => {{
      const editor = vscode.window.activeTextEditor;
      if (!editor) return;
      const selection = editor.document.getText(editor.selection);
      vscode.window.showInformationMessage('🔍 Hagibis: Analyzing architectural rationale...');
      try {{
        const res = await queryHgbd({{ ExplainAction: {{ intent: 'Editor Selection', diff_content: selection }} }});
        vscode.window.showInformationMessage('ADR Generated: ' + JSON.stringify(res));
      }} catch (e: any) {{
        vscode.window.showErrorMessage('hgbd error: ' + e.message);
      }}
    }})
  );
}}

export function deactivate() {{}}
"#,
            cfg.socket_path
        );

        let instructions = format!(
            "To install in VS Code or Cursor:\n1. Place in ~/.vscode/extensions/{}\n2. Run `npm install && npm run build`\n3. Reload VS Code with hgbd running at {}",
            cfg.extension_name, cfg.socket_path
        );

        VsCodeExtensionScaffoldReport {
            manifest_json,
            extension_ts_code,
            socket_path: cfg.socket_path,
            registered_commands: commands,
            install_instructions: instructions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vscode_scaffold() {
        let rep = VsCodeExtensionBridge::scaffold_extension(None);
        assert!(rep.manifest_json.contains("hgb.explainSelection"));
        assert!(rep.extension_ts_code.contains("/tmp/hgbd.sock"));
        assert_eq!(rep.registered_commands.len(), 6);
    }
}
