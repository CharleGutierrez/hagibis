//! # VisualRegressionSentry - Devin & Replit-Style Visual Layout Regression Oracle
//!
//! Elevates Devin and Replit's visual regression verification. Compares DOM
//! snapshots and bounding box metrics before and after frontend edits, computing
//! visual stability scores and flagging unintended layout shifts.

use serde::{Deserialize, Serialize};

/// Snapshot of a visual node at a given point in time
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualNodeSnapshot {
    pub tag: String,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub text_preview: Option<String>,
}

/// Type of visual delta detected between snapshots
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum VisualDeltaType {
    NodeDisappeared,
    NodeAdded,
    LayoutShift { dx: f64, dy: f64 },
    DimensionChange { dw: f64, dh: f64 },
    StyleDrift,
}

/// Detailed regression record for an individual DOM node
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualRegressionDelta {
    pub selector: String,
    pub delta_type: VisualDeltaType,
    pub is_breaking: bool,
    pub description: String,
}

/// Comprehensive visual regression report
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualRegressionReport {
    pub baseline_elements: usize,
    pub current_elements: usize,
    pub total_deltas: usize,
    pub breaking_shifts: usize,
    pub visual_stability_score: f64,
    pub is_visually_stable: bool,
    pub deltas: Vec<VisualRegressionDelta>,
    pub summary: String,
}

pub struct VisualRegressionSentry;

impl VisualRegressionSentry {
    /// Compare a baseline visual snapshot against current DOM state
    pub fn compare(
        baseline: &[VisualNodeSnapshot],
        current: &[VisualNodeSnapshot],
    ) -> VisualRegressionReport {
        let mut deltas = Vec::new();
        let mut breaking_count = 0;

        // Check for removed/shifted baseline elements
        for base in baseline {
            let selector = base
                .id
                .as_ref()
                .map(|i| format!("#{}", i))
                .or_else(|| {
                    if !base.classes.is_empty() {
                        Some(format!(".{}", base.classes[0]))
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| base.tag.clone());

            if let Some(curr) = current.iter().find(|c| c.tag == base.tag && c.id == base.id) {
                let dx = curr.x - base.x;
                let dy = curr.y - base.y;
                let dw = curr.width - base.width;
                let dh = curr.height - base.height;

                // Significant layout shift (>15px)
                if dx.abs() > 15.0 || dy.abs() > 15.0 {
                    let is_breaking = dy.abs() > 50.0;
                    if is_breaking {
                        breaking_count += 1;
                    }
                    deltas.push(VisualRegressionDelta {
                        selector: selector.clone(),
                        delta_type: VisualDeltaType::LayoutShift { dx, dy },
                        is_breaking,
                        description: format!("Layout shifted by ({:+.1}px, {:+.1}px)", dx, dy),
                    });
                }

                // Significant dimension change (>25%)
                if base.width > 0.0 && (dw.abs() / base.width) > 0.25 {
                    let is_breaking = dw < 0.0 && dw.abs() > 50.0;
                    if is_breaking {
                        breaking_count += 1;
                    }
                    deltas.push(VisualRegressionDelta {
                        selector: selector.clone(),
                        delta_type: VisualDeltaType::DimensionChange { dw, dh },
                        is_breaking,
                        description: format!("Dimensions resized by ({:+.1}px, {:+.1}px)", dw, dh),
                    });
                }
            } else {
                // Node disappeared entirely
                breaking_count += 1;
                deltas.push(VisualRegressionDelta {
                    selector: selector.clone(),
                    delta_type: VisualDeltaType::NodeDisappeared,
                    is_breaking: true,
                    description: "Element disappeared from visual render tree.".to_string(),
                });
            }
        }

        // Check for new unexpected nodes
        for curr in current {
            if !baseline.iter().any(|b| b.tag == curr.tag && b.id == curr.id) {
                deltas.push(VisualRegressionDelta {
                    selector: curr.id.as_ref().map(|i| format!("#{}", i)).unwrap_or_else(|| curr.tag.clone()),
                    delta_type: VisualDeltaType::NodeAdded,
                    is_breaking: false,
                    description: "New visual element inserted into DOM.".to_string(),
                });
            }
        }

        let total_deltas = deltas.len();
        let _total_nodes = baseline.len().max(1);
        let penalty = (breaking_count as f64 * 25.0) + ((total_deltas - breaking_count) as f64 * 5.0);
        let visual_stability_score = (100.0 - penalty).clamp(0.0, 100.0);
        let is_visually_stable = breaking_count == 0 && visual_stability_score >= 80.0;

        let summary = if is_visually_stable {
            format!("Visual Layout Stable ({:.1}% score). Zero breaking layout shifts detected.", visual_stability_score)
        } else {
            format!("Visual Regression Warning ({:.1}% score): {} breaking shifts detected.", visual_stability_score, breaking_count)
        };

        VisualRegressionReport {
            baseline_elements: baseline.len(),
            current_elements: current.len(),
            total_deltas,
            breaking_shifts: breaking_count,
            visual_stability_score,
            is_visually_stable,
            deltas,
            summary,
        }
    }
}
