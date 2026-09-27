//! # Multimodal Vision & Screenshot Ingestion Engine
//!
//! Bridges visual designs, Figma exports, clipboard screenshots (`Ctrl+V`),
//! and browser viewports directly into multimodal prompts (Google Gemini Cloud & local LLMs).

use crate::error::{HgbError, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};

/// Captured image format
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl ImageFormat {
    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
        }
    }
}

/// An ingested vision image payload with metadata
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisionImage {
    pub format: ImageFormat,
    pub mime_type: String,
    pub base64_data: String,
    pub byte_size: usize,
    pub blake3_hash: String,
    pub source_origin: String, // "clipboard", "screenshot", "file"
}

/// Multimodal prompt payload ready for dispatch to Gemini or vision-capable models
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultimodalPromptPayload {
    pub text_prompt: String,
    pub images: Vec<VisionImage>,
    pub auto_detected_intent: String,
}

pub struct MultimodalVisionEngine;

impl MultimodalVisionEngine {
    /// Ingest an image directly from system clipboard using platform-native utilities
    pub fn capture_from_clipboard() -> Result<VisionImage> {
        let mut raw_bytes: Option<Vec<u8>> = None;

        // 1. Wayland Linux (wl-paste -t image/png)
        if let Ok(output) = Command::new("wl-paste")
            .args(["-t", "image/png"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
        {
            if output.status.success() && !output.stdout.is_empty() {
                raw_bytes = Some(output.stdout);
            }
        }

        // 2. X11 Linux (xclip -selection clipboard -t image/png -o)
        if raw_bytes.is_none() {
            if let Ok(output) = Command::new("xclip")
                .args(["-selection", "clipboard", "-t", "image/png", "-o"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
            {
                if output.status.success() && !output.stdout.is_empty() {
                    raw_bytes = Some(output.stdout);
                }
            }
        }

        // 3. macOS (pngpaste -)
        if raw_bytes.is_none() {
            if let Ok(output) = Command::new("pngpaste")
                .arg("-")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
            {
                if output.status.success() && !output.stdout.is_empty() {
                    raw_bytes = Some(output.stdout);
                }
            }
        }

        let bytes = raw_bytes.ok_or_else(|| {
            HgbError::Execution("No image data found in clipboard. Copy an image or screenshot first.".to_string())
        })?;

        Self::from_bytes(&bytes, ImageFormat::Png, "clipboard")
    }

    /// Construct a VisionImage from raw bytes
    pub fn from_bytes(bytes: &[u8], format: ImageFormat, source_origin: &str) -> Result<VisionImage> {
        if bytes.is_empty() {
            return Err(HgbError::Execution("Empty image buffer received".to_string()));
        }

        let blake3_hash = blake3::hash(bytes).to_hex().to_string();
        let base64_data = base64::engine::general_purpose::STANDARD.encode(bytes);

        Ok(VisionImage {
            format,
            mime_type: format.mime_type().to_string(),
            base64_data,
            byte_size: bytes.len(),
            blake3_hash,
            source_origin: source_origin.to_string(),
        })
    }

    /// Construct a VisionImage from an existing base64 string
    pub fn from_base64(base64_str: &str, format: ImageFormat, source_origin: &str) -> Result<VisionImage> {
        let clean = base64_str.trim().trim_start_matches("data:image/png;base64,");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(clean)
            .map_err(|e| HgbError::Execution(format!("Invalid base64 image data: {}", e)))?;

        Self::from_bytes(&bytes, format, source_origin)
    }

    /// Assemble a multimodal prompt payload combining user instructions and captured images
    pub fn assemble_payload(user_prompt: &str, images: Vec<VisionImage>) -> MultimodalPromptPayload {
        let intent = if images.is_empty() {
            "text_only".to_string()
        } else if user_prompt.to_lowercase().contains("bug") || user_prompt.to_lowercase().contains("fix") {
            "visual_bug_diagnosis".to_string()
        } else if user_prompt.to_lowercase().contains("figma") || user_prompt.to_lowercase().contains("design") || user_prompt.to_lowercase().contains("ui") {
            "design_to_code".to_string()
        } else {
            "multimodal_vibe_edit".to_string()
        };

        MultimodalPromptPayload {
            text_prompt: user_prompt.to_string(),
            images,
            auto_detected_intent: intent,
        }
    }
}
