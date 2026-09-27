//! # Vibe-to-Spec Intent Expander
//!
//! Enriches concise vibe-coder prompts ("dark mode habit tracker with confetti")
//! into comprehensive design system tokens, typography scales, animation rules,
//! and component blueprints before code synthesis begins.

use serde::{Deserialize, Serialize};

/// Expanded design tokens inferred from the user's vibe
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DesignTokens {
    pub theme_name: String,
    pub background: String,
    pub surface: String,
    pub primary_accent: String,
    pub secondary_accent: String,
    pub text_primary: String,
    pub text_muted: String,
    pub border_style: String,
    pub blur_effect: String,
}

/// Motion and micro-interaction specifications
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MotionTokens {
    pub transition_curve: String,
    pub hover_scale: String,
    pub celebratory_effect: Option<String>,
    pub enter_animation: String,
}

/// Complete expanded specification for code generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandedVibeSpec {
    pub raw_prompt: String,
    pub design: DesignTokens,
    pub motion: MotionTokens,
    pub inferred_components: Vec<String>,
    pub recommended_packages: Vec<String>,
    pub enriched_system_prompt: String,
}

pub struct VibeIntentExpander;

impl VibeIntentExpander {
    /// Expand a vague vibe prompt into structured technical tokens
    pub fn expand(prompt: &str) -> ExpandedVibeSpec {
        let p = prompt.to_lowercase();

        // 1. Incur Theme Tokens
        let is_cyberpunk = p.contains("cyberpunk") || p.contains("neon");
        let is_light = p.contains("light mode") || p.contains("clean white") || p.contains("minimal");
        let is_retro = p.contains("retro") || p.contains("terminal") || p.contains("90s");

        let design = if is_cyberpunk {
            DesignTokens {
                theme_name: "Neon Cyberpunk".to_string(),
                background: "#05050a".to_string(),
                surface: "rgba(18, 18, 30, 0.85)".to_string(),
                primary_accent: "#00f0ff".to_string(),
                secondary_accent: "#ff007f".to_string(),
                text_primary: "#f0f8ff".to_string(),
                text_muted: "#64748b".to_string(),
                border_style: "1px solid rgba(0, 240, 255, 0.35)".to_string(),
                blur_effect: "backdrop-blur-md".to_string(),
            }
        } else if is_light {
            DesignTokens {
                theme_name: "Minimalist Studio".to_string(),
                background: "#f8fafc".to_string(),
                surface: "#ffffff".to_string(),
                primary_accent: "#2563eb".to_string(),
                secondary_accent: "#10b981".to_string(),
                text_primary: "#0f172a".to_string(),
                text_muted: "#64748b".to_string(),
                border_style: "1px solid #e2e8f0".to_string(),
                blur_effect: "backdrop-blur-sm".to_string(),
            }
        } else if is_retro {
            DesignTokens {
                theme_name: "Retro CRT Matrix".to_string(),
                background: "#0a0a0a".to_string(),
                surface: "#141414".to_string(),
                primary_accent: "#22c55e".to_string(),
                secondary_accent: "#eab308".to_string(),
                text_primary: "#4ade80".to_string(),
                text_muted: "#15803d".to_string(),
                border_style: "1px solid #22c55e".to_string(),
                blur_effect: "none".to_string(),
            }
        } else {
            // Modern Dark Glassmorphism Default
            DesignTokens {
                theme_name: "Modern Dark Glass".to_string(),
                background: "#090d16".to_string(),
                surface: "rgba(15, 23, 42, 0.75)".to_string(),
                primary_accent: "#38bdf8".to_string(),
                secondary_accent: "#818cf8".to_string(),
                text_primary: "#f8fafc".to_string(),
                text_muted: "#94a3b8".to_string(),
                border_style: "1px solid rgba(255, 255, 255, 0.08)".to_string(),
                blur_effect: "backdrop-blur-xl".to_string(),
            }
        };

        // 2. Incur Motion Tokens
        let has_confetti = p.contains("confetti") || p.contains("celebrat") || p.contains("streak");
        let motion = MotionTokens {
            transition_curve: "cubic-bezier(0.16, 1, 0.3, 1)".to_string(),
            hover_scale: "transform hover:scale-[1.02] active:scale-[0.98]".to_string(),
            celebratory_effect: if has_confetti { Some("canvas-confetti-blast".to_string()) } else { None },
            enter_animation: "animate-in fade-in zoom-in-95 duration-200".to_string(),
        };

        // 3. Inferred Components
        let mut components = Vec::new();
        if p.contains("tracker") || p.contains("habit") || p.contains("todo") {
            components.push("ActivityGrid".to_string());
            components.push("StreakBadge".to_string());
            components.push("QuickAddModal".to_string());
        }
        if p.contains("chart") || p.contains("dashboard") || p.contains("metric") {
            components.push("StatCard".to_string());
            components.push("AreaChartWidget".to_string());
        }
        if p.contains("auth") || p.contains("login") {
            components.push("OAuthButtonGroup".to_string());
            components.push("SessionGuard".to_string());
        }
        if components.is_empty() {
            components.push("HeaderNav".to_string());
            components.push("MainCanvas".to_string());
            components.push("ActionToolbar".to_string());
        }

        // 4. Recommended Packages
        let mut packages = vec!["lucide-react".to_string(), "tailwind-merge".to_string()];
        if has_confetti {
            packages.push("canvas-confetti".to_string());
        }
        if p.contains("animation") || p.contains("framer") {
            packages.push("framer-motion".to_string());
        }

        // 5. Synthesize Enriched System Prompt
        let enriched = format!(
            "VIBE SPECIFICATION ENRICHMENT:\n- Style Palette: {} (bg: {}, surface: {}, accent: {})\n- Motion: {} with {}\n- Component Hierarchy: {}\n- Target Libraries: {}\n\nImplementation Directive: Generate complete, fully-styled components respecting these aesthetic tokens. Avoid placeholder stubs.",
            design.theme_name,
            design.background,
            design.surface,
            design.primary_accent,
            motion.transition_curve,
            motion.enter_animation,
            components.join(" -> "),
            packages.join(", ")
        );

        ExpandedVibeSpec {
            raw_prompt: prompt.to_string(),
            design,
            motion,
            inferred_components: components,
            recommended_packages: packages,
            enriched_system_prompt: enriched,
        }
    }
}
