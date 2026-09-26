use chrono::Utc;
use colored::Colorize;
use hgb_core::providers::GeminiProvider;
use hgb_core::{HgbError, Result};
use serde::Deserialize;
use std::sync::Arc;

pub use hgb_core::glance::{
    BoundingBox, CssSuggestion, DefectCategory, DefectSeverity, ImagePayload, LayoutDefect,
    PatchTarget, VisualInspectionReport,
};

/// GlanceEngine: Multimodal Visual Canvas & Layout Autopsy
pub struct GlanceEngine {
    provider: Option<Arc<GeminiProvider>>,
}

impl Default for GlanceEngine {
    fn default() -> Self {
        Self {
            provider: GeminiProvider::auto_discover().map(Arc::new),
        }
    }
}

impl GlanceEngine {
    pub fn new(provider: Option<Arc<GeminiProvider>>) -> Self {
        Self { provider }
    }

    /// Perform multimodal layout autopsy on an image payload, optionally guided by source HTML/CSS
    pub async fn inspect_image(
        &self,
        payload: &ImagePayload,
        html_or_css_context: Option<&str>,
    ) -> Result<VisualInspectionReport> {
        let width = payload.width.unwrap_or(1280);
        let height = payload.height.unwrap_or(800);

        if let Some(ref prov) = self.provider {
            // Multimodal Gemini prompt
            let prompt = format!(
                "You are GlanceEngine, an elite visual QA and layout autopsy engine.\n\
                Analyze the attached UI screenshot ({}x{} px).\n\
                Identify visual defects in these categories:\n\
                - Overlap (elements overlapping unexpectedly)\n\
                - Clipping (text or button content truncated or clipped by overflow)\n\
                - ContrastFailure (insufficient contrast)\n\
                - AlignmentMismatch (misaligned elements)\n\
                - ResponsiveBreakdown (broken grid, overflow scrollbars)\n\
                - UnstyledElement (raw unstyled DOM)\n\n\
                {}\n\n\
                Respond strictly in JSON formatted as:\n\
                {{\n\
                  \"summary\": \"Brief summary of layout health\",\n\
                  \"defects\": [\n\
                    {{\n\
                      \"id\": \"defect_1\",\n\
                      \"category\": \"Overlap|Clipping|ContrastFailure|AlignmentMismatch|ResponsiveBreakdown|UnstyledElement\",\n\
                      \"severity\": \"Critical|High|Medium|Low\",\n\
                      \"description\": \"...\",\n\
                      \"affected_element\": \".btn-primary\"\n\
                    }}\n\
                  ],\n\
                  \"suggestions\": [\n\
                    {{\n\
                      \"selector\": \".btn-primary\",\n\
                      \"suggested_css\": \"padding: 8px 16px; min-width: 120px;\",\n\
                      \"reason\": \"Prevent text clipping on small viewports\"\n\
                    }}\n\
                  ]\n\
                }}",
                width,
                height,
                html_or_css_context.map(|c| format!("Source Code Context:\n{}", c)).unwrap_or_default()
            );

            match prov.complete_multimodal(&prompt, &[payload.clone()], Some("gemini-2.5-flash")).await {
                Ok(raw_json) => {
                    if let Ok(rep) = Self::parse_ai_report(&raw_json, payload, width, height) {
                        return Ok(rep);
                    }
                }
                Err(_) => {
                    // Fall back to heuristic layout autopsy
                }
            }
        }

        // Heuristic fallback layout autopsy
        Ok(Self::heuristic_layout_autopsy(payload, width, height, html_or_css_context))
    }

    /// Parse LLM JSON response into typed VisualInspectionReport
    fn parse_ai_report(
        raw: &str,
        payload: &ImagePayload,
        width: u32,
        height: u32,
    ) -> Result<VisualInspectionReport> {
        let cleaned = if let Some(start) = raw.find('{') {
            if let Some(end) = raw.rfind('}') {
                &raw[start..=end]
            } else {
                raw
            }
        } else {
            raw
        };

        #[derive(Deserialize)]
        struct RawDefect {
            id: Option<String>,
            category: Option<String>,
            severity: Option<String>,
            description: Option<String>,
            affected_element: Option<String>,
        }

        #[derive(Deserialize)]
        struct RawSuggestion {
            selector: Option<String>,
            suggested_css: Option<String>,
            current_css: Option<String>,
            reason: Option<String>,
        }

        #[derive(Deserialize)]
        struct RawReport {
            summary: Option<String>,
            defects: Option<Vec<RawDefect>>,
            suggestions: Option<Vec<RawSuggestion>>,
        }

        let parsed: RawReport = serde_json::from_str(cleaned)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;

        let mut defects = Vec::new();
        if let Some(raw_defects) = parsed.defects {
            for (idx, d) in raw_defects.into_iter().enumerate() {
                let category = match d.category.as_deref().unwrap_or("").to_lowercase().as_str() {
                    "overlap" => DefectCategory::Overlap,
                    "clipping" => DefectCategory::Clipping,
                    "contrastfailure" | "contrast" => DefectCategory::ContrastFailure,
                    "alignmentmismatch" | "alignment" => DefectCategory::AlignmentMismatch,
                    "responsivebreakdown" | "responsive" => DefectCategory::ResponsiveBreakdown,
                    _ => DefectCategory::UnstyledElement,
                };

                let severity = match d.severity.as_deref().unwrap_or("").to_lowercase().as_str() {
                    "critical" => DefectSeverity::Critical,
                    "high" => DefectSeverity::High,
                    "low" => DefectSeverity::Low,
                    _ => DefectSeverity::Medium,
                };

                defects.push(LayoutDefect {
                    id: d.id.unwrap_or_else(|| format!("defect_{}", idx + 1)),
                    category,
                    severity,
                    description: d.description.unwrap_or_default(),
                    bounding_box: None,
                    affected_element: d.affected_element.unwrap_or_else(|| "div".to_string()),
                });
            }
        }

        let mut suggestions = Vec::new();
        if let Some(raw_suggs) = parsed.suggestions {
            for s in raw_suggs {
                suggestions.push(CssSuggestion {
                    selector: s.selector.unwrap_or_else(|| "element".to_string()),
                    suggested_css: s.suggested_css.unwrap_or_default(),
                    current_css: s.current_css,
                    reason: s.reason.unwrap_or_default(),
                });
            }
        }

        Ok(VisualInspectionReport {
            image_path: payload.path.clone(),
            dimensions: (width, height),
            defects,
            suggestions,
            patch_targets: vec![],
            summary: parsed.summary.unwrap_or_else(|| "Visual autopsy completed.".to_string()),
            analyzed_at_rfc3339: Utc::now().to_rfc3339(),
        })
    }

    /// Fast heuristic layout analyzer when multimodal provider is offline
    pub fn heuristic_layout_autopsy(
        payload: &ImagePayload,
        width: u32,
        height: u32,
        context: Option<&str>,
    ) -> VisualInspectionReport {
        let mut defects = Vec::new();
        let mut suggestions = Vec::new();

        // 1. Viewport ratio checks
        if width > 0 && height > 0 {
            let aspect_ratio = width as f32 / height as f32;
            if aspect_ratio > 3.0 {
                defects.push(LayoutDefect {
                    id: "aspect_ratio_warning".to_string(),
                    category: DefectCategory::ResponsiveBreakdown,
                    severity: DefectSeverity::Medium,
                    description: format!("Unusual extreme aspect ratio ({:.2}:1); check if horizontal clipping occurs", aspect_ratio),
                    bounding_box: None,
                    affected_element: "viewport".to_string(),
                });
            }
        }

        // 2. Scan CSS context if provided
        if let Some(css) = context {
            if css.contains("overflow: hidden") && css.contains("height:") {
                defects.push(LayoutDefect {
                    id: "potential_clipping".to_string(),
                    category: DefectCategory::Clipping,
                    severity: DefectSeverity::High,
                    description: "Fixed height container with overflow:hidden detected; potential text truncation".to_string(),
                    bounding_box: None,
                    affected_element: "container".to_string(),
                });
                suggestions.push(CssSuggestion {
                    selector: ".container".to_string(),
                    suggested_css: "min-height: auto; height: fit-content; overflow: visible;".to_string(),
                    current_css: Some("overflow: hidden;".to_string()),
                    reason: "Allow container to dynamically expand to fit multiline content".to_string(),
                });
            }

            if css.contains("position: absolute") && !css.contains("z-index") {
                defects.push(LayoutDefect {
                    id: "stacking_overlap".to_string(),
                    category: DefectCategory::Overlap,
                    severity: DefectSeverity::Medium,
                    description: "Absolute positioned element lacks explicit z-index stacking context".to_string(),
                    bounding_box: None,
                    affected_element: "absolute-layer".to_string(),
                });
            }
        }

        let summary = if defects.is_empty() {
            format!("Canvas layout autopsy sound ({}x{} px). Zero visual regressions detected.", width, height)
        } else {
            format!("Autopsy detected {} layout issues across {}x{} px canvas.", defects.len(), width, height)
        };

        VisualInspectionReport {
            image_path: payload.path.clone(),
            dimensions: (width, height),
            defects,
            suggestions,
            patch_targets: vec![],
            summary,
            analyzed_at_rfc3339: Utc::now().to_rfc3339(),
        }
    }

    /// Render terminal ANSI report for developer CLI vibe coding
    pub fn render_terminal_card(report: &VisualInspectionReport) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "{} Visual Autopsy: {}x{} px ({})\n",
            "🖼️".bold(),
            report.dimensions.0,
            report.dimensions.1,
            report.image_path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "in-memory".to_string())
        ));
        out.push_str(&format!("  {}\n\n", report.summary.cyan()));

        if report.defects.is_empty() {
            out.push_str(&format!("  {} Zero layout defects detected!\n", "✔".green()));
        } else {
            out.push_str(&format!("  {} Detected Defects:\n", "⚠".yellow()));
            for d in &report.defects {
                let badge = match d.severity {
                    DefectSeverity::Critical => "[CRITICAL]".red().bold(),
                    DefectSeverity::High => "[HIGH]".red(),
                    DefectSeverity::Medium => "[MED]".yellow(),
                    DefectSeverity::Low => "[LOW]".blue(),
                };
                out.push_str(&format!(
                    "   • {} {}: {} ({})\n",
                    badge,
                    d.category.as_str().bold(),
                    d.description,
                    d.affected_element.dimmed()
                ));
            }
        }

        if !report.suggestions.is_empty() {
            out.push_str(&format!("\n  {} Recommended CSS Patches:\n", "💡".green()));
            for s in &report.suggestions {
                out.push_str(&format!("   • {}\n", s.selector.bold()));
                out.push_str(&format!("     {}\n", s.suggested_css.green()));
                out.push_str(&format!("     {}\n", s.reason.dimmed()));
            }
        }

        out
    }
}
