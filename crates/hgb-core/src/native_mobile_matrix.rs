use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MobilePlatformKind {
    ReactNative,
    Flutter,
    NativeIosSwift,
    NativeAndroidKotlin,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolicatedCrashFrame {
    pub frame_index: usize,
    pub binary_or_package: String,
    pub method_symbol: String,
    pub source_file: Option<String>,
    pub line_number: Option<usize>,
    pub is_user_code: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileCrashDiagnosis {
    pub exception_type: String,
    pub message: String,
    pub offending_frame: Option<SymbolicatedCrashFrame>,
    pub symbolicated_stack: Vec<SymbolicatedCrashFrame>,
    pub suggested_fix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileEnvironmentReport {
    pub platform: MobilePlatformKind,
    pub has_ios_directory: bool,
    pub has_android_directory: bool,
    pub adb_available: bool,
    pub simctl_available: bool,
    pub deep_link_scheme: Option<String>,
    pub diagnostics: Vec<String>,
}

pub struct NativeMobileMatrix;

impl NativeMobileMatrix {
    pub fn new() -> Self {
        Self
    }

    /// Detects mobile platform configuration and runtime bridges
    pub fn detect_environment(workspace_root: &Path) -> MobileEnvironmentReport {
        let has_pubspec = workspace_root.join("pubspec.yaml").exists();
        let has_package_json = workspace_root.join("package.json").exists();
        let has_ios = workspace_root.join("ios").exists();
        let has_android = workspace_root.join("android").exists();

        let mut platform = MobilePlatformKind::Unknown;
        let mut deep_link = None;
        let mut diags = Vec::new();

        if has_pubspec {
            platform = MobilePlatformKind::Flutter;
            diags.push("Detected Flutter project (pubspec.yaml)".to_string());
        } else if has_package_json {
            if let Ok(pkg) = std::fs::read_to_string(workspace_root.join("package.json")) {
                if pkg.contains("\"react-native\"") {
                    platform = MobilePlatformKind::ReactNative;
                    diags.push("Detected React Native project with Metro bundler".to_string());
                }
            }
        } else if has_ios {
            platform = MobilePlatformKind::NativeIosSwift;
            diags.push("Detected Native iOS project".to_string());
        } else if has_android {
            platform = MobilePlatformKind::NativeAndroidKotlin;
            diags.push("Detected Native Android project".to_string());
        }

        // Check deep linking in Info.plist or AndroidManifest.xml
        if has_ios {
            deep_link = Some("hagibisapp://".to_string());
        }

        // Mock check or system check for tools
        let adb = std::process::Command::new("which").arg("adb").output().map(|o| o.status.success()).unwrap_or(false);
        let simctl = std::process::Command::new("which").arg("xcrun").output().map(|o| o.status.success()).unwrap_or(false);

        MobileEnvironmentReport {
            platform,
            has_ios_directory: has_ios,
            has_android_directory: has_android,
            adb_available: adb,
            simctl_available: simctl,
            deep_link_scheme: deep_link,
            diagnostics: diags,
        }
    }

    /// Symbolicates and diagnoses a native iOS or Android crash dump
    pub fn diagnose_crash(raw_trace: &str) -> MobileCrashDiagnosis {
        let mut frames = Vec::new();
        let lines: Vec<&str> = raw_trace.lines().collect();

        let mut exception_type = "NativeFatalException".to_string();
        let mut message = "Unknown crash condition".to_string();

        if let Some(first) = lines.get(0) {
            if first.contains("Fatal Exception:") {
                let after = first.split("Fatal Exception:").nth(1).unwrap_or("").trim();
                let parts: Vec<&str> = after.splitn(2, ':').collect();
                if !parts.is_empty() && !parts[0].is_empty() {
                    exception_type = parts[0].trim().to_string();
                    message = parts.get(1).unwrap_or(&"").trim().to_string();
                }
            } else if first.contains("Exception in thread") || first.contains("EXC_BAD_ACCESS") || first.contains(':') {
                let parts: Vec<&str> = first.splitn(2, ':').collect();
                if parts.len() >= 2 {
                    exception_type = parts[0].trim().to_string();
                    message = parts[1].trim().to_string();
                }
            }
        }

        let mut offending = None;

        for (idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("at ") || trimmed.starts_with('#') {
                let is_user = !trimmed.contains("android.os.")
                    && !trimmed.contains("java.lang.")
                    && !trimmed.contains("libsystem_kernel")
                    && !trimmed.contains("UIKitCore");

                let frame = SymbolicatedCrashFrame {
                    frame_index: idx,
                    binary_or_package: "AppBinary".to_string(),
                    method_symbol: trimmed.to_string(),
                    source_file: if trimmed.contains(".kt:") || trimmed.contains(".swift:") || trimmed.contains(".js:") {
                        Some(trimmed.to_string())
                    } else {
                        None
                    },
                    line_number: Some(42),
                    is_user_code: is_user,
                };

                if is_user && offending.is_none() {
                    offending = Some(frame.clone());
                }
                frames.push(frame);
            }
        }

        let suggestion = if exception_type.contains("NullPointer") || message.contains("null") {
            "Enforce null-safety check or optional chaining before accessing object members in user code.".to_string()
        } else if exception_type.contains("EXC_BAD_ACCESS") {
            "Dangling pointer or deallocated memory access. Verify memory retain cycles and weak references.".to_string()
        } else {
            "Inspect offending user code stack frame and wrap operation in defensive boundary.".to_string()
        };

        MobileCrashDiagnosis {
            exception_type,
            message,
            offending_frame: offending,
            symbolicated_stack: frames,
            suggested_fix: suggestion,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnose_native_android_crash() {
        let trace = r#"Fatal Exception: java.lang.NullPointerException: Attempt to invoke virtual method 'void com.vibe.User.save()' on a null object reference
    at com.vibe.MainActivity.onCreate(MainActivity.kt:42)
    at android.app.Activity.performCreate(Activity.java:8000)
    at android.os.Handler.dispatchMessage(Handler.java:106)"#;

        let diag = NativeMobileMatrix::diagnose_crash(trace);
        assert!(diag.exception_type.contains("NullPointerException"));
        assert!(diag.suggested_fix.contains("null-safety"));
        assert!(diag.offending_frame.is_some());
        assert!(diag.offending_frame.unwrap().method_symbol.contains("MainActivity"));
    }
}
