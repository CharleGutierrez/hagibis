use hgb_core::cdp_tweak_mirror::{CdpTweakMirror, DomTweakEvent};
use hgb_core::compiler_interceptor::{CompilerDiagnostic, CompilerInterceptor, CompilerTool, DiagnosticSeverity};
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn test_typescript_diagnostics_parsing() {
    let tsc_output = r#"
src/index.ts:15:7 - error TS2322: Type 'string' is not assignable to type 'number'.
src/components/Button.tsx(24,10): error TS2304: Cannot find name 'foo'.
src/utils.ts:42:1: error TS1005: ';' expected.
src/app.ts:5:2 - warning TS7006: Parameter 'x' implicitly has an 'any' type.
"#;
    let diags = CompilerInterceptor::parse_typescript_diagnostics("", tsc_output);
    assert_eq!(diags.len(), 4);

    // Diagnostic 1
    assert_eq!(diags[0].tool, CompilerTool::TypeScript);
    assert_eq!(diags[0].severity, DiagnosticSeverity::Error);
    assert_eq!(diags[0].error_code.as_deref(), Some("TS2322"));
    assert_eq!(diags[0].file, PathBuf::from("src/index.ts"));
    assert_eq!(diags[0].line, 15);
    assert_eq!(diags[0].column, 7);
    assert!(diags[0].message.contains("Type 'string' is not assignable"));

    // Diagnostic 2
    assert_eq!(diags[1].file, PathBuf::from("src/components/Button.tsx"));
    assert_eq!(diags[1].line, 24);
    assert_eq!(diags[1].column, 10);
    assert_eq!(diags[1].error_code.as_deref(), Some("TS2304"));
    assert!(diags[1].suggested_fix.as_deref().unwrap().contains("identifier"));

    // Diagnostic 3
    assert_eq!(diags[2].file, PathBuf::from("src/utils.ts"));
    assert_eq!(diags[2].line, 42);
    assert_eq!(diags[2].column, 1);
    assert_eq!(diags[2].error_code.as_deref(), Some("TS1005"));
    assert!(diags[2].suggested_fix.as_deref().unwrap().contains("semicolon"));

    // Diagnostic 4 (warning)
    assert_eq!(diags[3].severity, DiagnosticSeverity::Warning);
    assert_eq!(diags[3].error_code.as_deref(), Some("TS7006"));
}

#[test]
fn test_go_diagnostics_parsing() {
    let go_output = r#"
./main.go:12:2: undefined: fmt.Printl
pkg/service.go:28:14: syntax error: unexpected newline, expecting comma or }
server.go:15:6: missing return at end of function
# example.com/pkg
"#;
    let diags = CompilerInterceptor::parse_go_diagnostics(go_output, "");
    assert_eq!(diags.len(), 3);

    // 1. undefined symbol
    assert_eq!(diags[0].tool, CompilerTool::Go);
    assert_eq!(diags[0].file, PathBuf::from("main.go"));
    assert_eq!(diags[0].line, 12);
    assert_eq!(diags[0].column, 2);
    assert_eq!(diags[0].error_code.as_deref(), Some("undefined"));

    // 2. syntax error missing comma
    assert_eq!(diags[1].file, PathBuf::from("pkg/service.go"));
    assert_eq!(diags[1].line, 28);
    assert_eq!(diags[1].column, 14);
    assert!(diags[1].suggested_fix.as_deref().unwrap().contains("comma"));

    // 3. missing return
    assert_eq!(diags[2].file, PathBuf::from("server.go"));
    assert_eq!(diags[2].line, 15);
    assert_eq!(diags[2].column, 6);
    assert!(diags[2].suggested_fix.as_deref().unwrap().contains("return"));
}

#[test]
fn test_biome_eslint_diagnostics_parsing() {
    let biome_output = "src/index.ts:10:5 lint/suspicious/noExplicitAny: Unexpected any. Specify a different type.\n";
    let diags = CompilerInterceptor::parse_biome_eslint_diagnostics(biome_output, "");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].tool, CompilerTool::TypeScript);
    assert_eq!(diags[0].line, 10);
    assert_eq!(diags[0].column, 5);
    assert_eq!(diags[0].error_code.as_deref(), Some("lint/suspicious/noExplicitAny"));

    let eslint_output = "src/index.js:10:5: Unexpected var, use let or const instead. [Error/no-var]\nsrc/components/Header.tsx:12:1: 'logo' is defined but never used. [Warning/@typescript-eslint/no-unused-vars]\n";
    let es_diags = CompilerInterceptor::parse_biome_eslint_diagnostics(eslint_output, "");
    assert_eq!(es_diags.len(), 2);
    assert_eq!(es_diags[0].tool, CompilerTool::JavaScript);
    assert_eq!(es_diags[0].severity, DiagnosticSeverity::Error);
    assert_eq!(es_diags[0].error_code.as_deref(), Some("no-var"));

    assert_eq!(es_diags[1].tool, CompilerTool::TypeScript);
    assert_eq!(es_diags[1].severity, DiagnosticSeverity::Warning);
    assert_eq!(es_diags[1].error_code.as_deref(), Some("@typescript-eslint/no-unused-vars"));
}

#[test]
fn test_polyglot_manifest_detection_all_ecosystems() {
    let tmp = std::env::temp_dir().join(format!("hgb_polyglot_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let _ = fs::create_dir_all(&tmp);

    // 1. Rust Cargo.toml
    fs::write(tmp.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();
    let (tool, bin, args) = CompilerInterceptor::auto_detect_polyglot_check(&tmp, None);
    assert_eq!(tool, CompilerTool::Rust);
    assert_eq!(bin, "cargo");
    assert!(args.contains(&"check".to_string()));
    let _ = fs::remove_file(tmp.join("Cargo.toml"));

    // 2. Go go.mod
    fs::write(tmp.join("go.mod"), "module example.com/test\n\ngo 1.22\n").unwrap();
    let (tool, bin, args) = CompilerInterceptor::auto_detect_polyglot_check(&tmp, None);
    assert_eq!(tool, CompilerTool::Go);
    assert_eq!(bin, "go");
    assert_eq!(args, vec!["vet", "./..."]);
    let _ = fs::remove_file(tmp.join("go.mod"));

    // 3. TypeScript tsconfig.json
    fs::write(tmp.join("tsconfig.json"), "{}\n").unwrap();
    let (tool, bin, args) = CompilerInterceptor::auto_detect_polyglot_check(&tmp, None);
    assert_eq!(tool, CompilerTool::TypeScript);
    assert_eq!(bin, "tsc");
    assert_eq!(args, vec!["--noEmit"]);
    let _ = fs::remove_file(tmp.join("tsconfig.json"));

    // 4. Bun in lockfile
    fs::write(tmp.join("bun.lockb"), "").unwrap();
    let (tool, bin, args) = CompilerInterceptor::auto_detect_polyglot_check(&tmp, Some(Path::new("src/index.ts")));
    assert_eq!(tool, CompilerTool::TypeScript);
    assert_eq!(bin, "bun");
    assert_eq!(args, vec!["run", "check"]);
    let _ = fs::remove_file(tmp.join("bun.lockb"));

    // 5. Biome in biome.json
    fs::write(tmp.join("biome.json"), "{}").unwrap();
    let (tool, bin, args) = CompilerInterceptor::auto_detect_polyglot_check(&tmp, Some(Path::new("src/App.tsx")));
    assert_eq!(tool, CompilerTool::TypeScript);
    assert_eq!(bin, "biome");
    assert_eq!(args, vec!["check"]);
    let _ = fs::remove_file(tmp.join("biome.json"));

    // 6. Python ruff / pyproject.toml
    fs::write(tmp.join("pyproject.toml"), "[tool.ruff]\n").unwrap();
    let (tool, bin, args) = CompilerInterceptor::auto_detect_polyglot_check(&tmp, Some(Path::new("script.py")));
    assert_eq!(tool, CompilerTool::Python);
    assert_eq!(bin, "ruff");
    assert_eq!(args, vec!["check"]);
    let _ = fs::remove_file(tmp.join("pyproject.toml"));

    // 7. Non-Rust fallback: verify cargo check is NEVER called when Cargo.toml is absent!
    let (tool, bin, _args) = CompilerInterceptor::auto_detect_polyglot_check(&tmp, Some(Path::new("unknown.xyz")));
    assert_ne!(bin, "cargo");
    assert_eq!(tool, CompilerTool::Unknown);

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_jsx_ast_aware_matching_preventing_collisions() {
    let tsx_source = r#"
export function Header() {
  return (
    <header className="flex items-center justify-between p-4 bg-white">
      <div className="flex items-center space-x-2">Logo</div>
      <button className="flex items-center text-sm text-gray-700 hover:text-black">Sign In</button>
    </header>
  );
}

export function Sidebar() {
  return (
    <aside className="w-64 bg-gray-50">
      <button className="flex items-center text-sm text-blue-600 hover:text-blue-800">Dashboard</button>
    </aside>
  );
}
"#;
    let tmp = std::env::temp_dir().join(format!("cdp_ast_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let _ = fs::create_dir_all(&tmp);
    let test_file = tmp.join("App.tsx");
    fs::write(&test_file, tsx_source).unwrap();

    let mirror = CdpTweakMirror::new(&tmp);

    // Target the button in Sidebar specifically
    let event = DomTweakEvent {
        selector: "button.text-blue-600".to_string(),
        property_or_attr: "className".to_string(),
        old_value: "text-blue-600".to_string(),
        new_value: "text-blue-900 font-bold".to_string(),
        component_hint: Some("Sidebar".to_string()),
        file_hint: Some("App.tsx".to_string()),
    };

    let report = mirror.sync_tweak(&event, true).expect("sync tweak");
    assert!(report.success);

    let updated = fs::read_to_string(&test_file).unwrap();

    // Verify Sidebar button was surgically updated
    assert!(updated.contains("text-blue-900 font-bold"));
    // Verify Header button was NOT accidentally overwritten even though it shares `flex items-center text-sm`!
    assert!(updated.contains("className=\"flex items-center text-sm text-gray-700 hover:text-black\""));
    // Verify Header div and container were untouched
    assert!(updated.contains("<header className=\"flex items-center justify-between p-4 bg-white\">"));
    assert!(updated.contains("<div className=\"flex items-center space-x-2\">"));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_jsx_multiline_formatting_preservation() {
    let tsx_source = r#"
export function CheckoutButton() {
  return (
    <button
      id="checkout-btn"
      className="flex items-center bg-blue-500 hover:bg-blue-600 px-4 py-2"
      disabled={false}
      onClick={handleCheckout}
    >
      Proceed to Checkout
    </button>
  );
}
"#;
    let tmp = std::env::temp_dir().join(format!("cdp_multi_fmt_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let _ = fs::create_dir_all(&tmp);
    let test_file = tmp.join("Checkout.tsx");
    fs::write(&test_file, tsx_source).unwrap();

    let mirror = CdpTweakMirror::new(&tmp);

    let event = DomTweakEvent {
        selector: "button#checkout-btn".to_string(),
        property_or_attr: "className".to_string(),
        old_value: "bg-blue-500".to_string(),
        new_value: "bg-emerald-600 shadow-lg".to_string(),
        component_hint: Some("CheckoutButton".to_string()),
        file_hint: Some("Checkout.tsx".to_string()),
    };

    let report = mirror.sync_tweak(&event, true).expect("sync multiline");
    assert!(report.success);

    let updated = fs::read_to_string(&test_file).unwrap();
    assert!(updated.contains("bg-emerald-600 shadow-lg hover:bg-blue-600"));
    assert!(updated.contains("id=\"checkout-btn\""));
    assert!(updated.contains("disabled={false}"));
    assert!(updated.contains("onClick={handleCheckout}"));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_autonomous_heal_patch_polyglot() {
    // TypeScript missing semicolon
    let ts_code = "const a: number = 10\nconst b: number = 20;\n";
    let ts_diag = CompilerDiagnostic {
        tool: CompilerTool::TypeScript,
        severity: DiagnosticSeverity::Error,
        file: PathBuf::from("index.ts"),
        line: 1,
        column: 21,
        error_code: Some("TS1005".to_string()),
        message: "';' expected.".to_string(),
        suggested_fix: None,
    };
    let healed_ts = CompilerInterceptor::autonomous_heal_patch(CompilerTool::TypeScript, ts_code, &[ts_diag]);
    assert!(healed_ts.is_some());
    assert!(healed_ts.unwrap().contains("const a: number = 10;"));

    // Go missing comma
    let go_code = "var m = map[string]int{\n    \"x\": 42\n}\n";
    let go_diag = CompilerDiagnostic {
        tool: CompilerTool::Go,
        severity: DiagnosticSeverity::Error,
        file: PathBuf::from("main.go"),
        line: 2,
        column: 12,
        error_code: None,
        message: "syntax error: unexpected newline, expecting comma or }".to_string(),
        suggested_fix: None,
    };
    let healed_go = CompilerInterceptor::autonomous_heal_patch(CompilerTool::Go, go_code, &[go_diag]);
    assert!(healed_go.is_some());
    assert!(healed_go.unwrap().contains("\"x\": 42,"));
}
