use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use crate::error::{HgbError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageMimeType {
    Png,
    Jpeg,
    WebP,
    Gif,
    Svg,
}

impl ImageMimeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
            Self::Svg => "image/svg+xml",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefectCategory {
    Overlap,
    Clipping,
    ContrastFailure,
    AlignmentMismatch,
    ResponsiveBreakdown,
    UnstyledElement,
}

impl DefectCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Overlap => "Overlap",
            Self::Clipping => "Clipping",
            Self::ContrastFailure => "Contrast Failure",
            Self::AlignmentMismatch => "Alignment Mismatch",
            Self::ResponsiveBreakdown => "Responsive Breakdown",
            Self::UnstyledElement => "Unstyled Element",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DefectSeverity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BoundingBox {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LayoutDefect {
    pub id: String,
    pub category: DefectCategory,
    pub severity: DefectSeverity,
    pub description: String,
    pub bounding_box: Option<BoundingBox>,
    pub affected_element: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CssSuggestion {
    pub selector: String,
    pub suggested_css: String,
    pub current_css: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchTarget {
    pub file_path: PathBuf,
    pub start_line: usize,
    pub end_line: usize,
    pub patch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualInspectionReport {
    pub image_path: Option<PathBuf>,
    pub dimensions: (u32, u32),
    pub defects: Vec<LayoutDefect>,
    pub suggestions: Vec<CssSuggestion>,
    pub patch_targets: Vec<PatchTarget>,
    pub summary: String,
    pub analyzed_at_rfc3339: String,
}

/// In-memory binary image representation with MIME detection and base64 encoding
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImagePayload {
    pub path: Option<PathBuf>,
    pub mime_type: String,
    pub base64_data: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size_bytes: usize,
    pub digest: String,
}

impl ImagePayload {
    /// Load an image from a filesystem path
    pub fn load_from_path<P: AsRef<Path>>(path: P) -> Result<Self> {
        let p = path.as_ref();
        if !p.exists() {
            return Err(HgbError::NotFound(format!("Image not found: {}", p.display())));
        }
        let bytes = fs::read(p)?;
        let hint_mime = p.extension().and_then(|e| e.to_str()).and_then(|ext| match ext.to_lowercase().as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "webp" => Some("image/webp"),
            "gif" => Some("image/gif"),
            "svg" => Some("image/svg+xml"),
            _ => None,
        });
        let mut payload = Self::from_bytes(bytes, hint_mime)?;
        payload.path = Some(p.to_path_buf());
        Ok(payload)
    }

    /// Construct ImagePayload from raw bytes, detecting MIME type and dimensions via magic bytes
    pub fn from_bytes(bytes: Vec<u8>, hint_mime: Option<&str>) -> Result<Self> {
        if bytes.is_empty() {
            return Err(HgbError::Execution("Image payload buffer is empty".to_string()));
        }
        let mime = detect_mime_type(&bytes)
            .or_else(|| hint_mime.map(String::from))
            .unwrap_or_else(|| "application/octet-stream".to_string());

        let (width, height) = extract_dimensions(&bytes, &mime);
        let base64_data = BASE64.encode(&bytes);
        let size_bytes = bytes.len();

        let mut hasher = Hasher::new();
        hasher.update(&bytes);
        let digest = hasher.finalize().to_hex().to_string();

        Ok(Self {
            path: None,
            mime_type: mime,
            base64_data,
            width,
            height,
            size_bytes,
            digest,
        })
    }

    /// Render standard data URI for HTML/Canvas embedding: `data:<mime>;base64,<data>`
    pub fn to_data_uri(&self) -> String {
        format!("data:{};base64,{}", self.mime_type, self.base64_data)
    }
}

/// Detect image MIME type using signature magic bytes
pub fn detect_mime_type(data: &[u8]) -> Option<String> {
    // PNG: 89 50 4E 47 0D 0A 1A 0A
    if data.len() >= 8 && data[0..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some("image/png".to_string());
    }
    // JPEG: FF D8 FF
    if data.len() >= 3 && data[0..3] == [0xFF, 0xD8, 0xFF] {
        return Some("image/jpeg".to_string());
    }
    // WebP: RIFF .... WEBP
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Some("image/webp".to_string());
    }
    // GIF: GIF87a or GIF89a
    if data.len() >= 6 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        return Some("image/gif".to_string());
    }
    // SVG text inspection
    let sample = &data[..std::cmp::min(data.len(), 256)];
    if let Ok(text) = std::str::from_utf8(sample) {
        let trimmed = text.trim_start();
        if trimmed.starts_with("<svg") || (trimmed.starts_with("<?xml") && trimmed.contains("<svg")) {
            return Some("image/svg+xml".to_string());
        }
    }
    None
}

/// Extract width and height dimensions from binary image headers without heavy decoders
pub fn extract_dimensions(data: &[u8], mime: &str) -> (Option<u32>, Option<u32>) {
    match mime {
        "image/png" => {
            // PNG IHDR chunk is located at offset 12..28 (12..16 = IHDR, 16..20 = width, 20..24 = height)
            if data.len() >= 24 && &data[12..16] == b"IHDR" {
                let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
                let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
                (Some(w), Some(h))
            } else {
                (None, None)
            }
        }
        "image/jpeg" => {
            // Scan JPEG segments for SOF markers (Start Of Frame): 0xFFC0 (baseline), 0xFFC1 (extended), 0xFFC2 (progressive)
            let mut i = 2;
            while i + 4 < data.len() {
                if data[i] != 0xFF {
                    i += 1;
                    continue;
                }
                let marker = data[i + 1];
                if marker == 0xC0 || marker == 0xC1 || marker == 0xC2 {
                    if i + 9 < data.len() {
                        let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                        let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                        return (Some(w), Some(h));
                    }
                    break;
                }
                // Stop if we hit SOS (Start of Scan 0xDA) or EOI (0xD9)
                if marker == 0xD9 || marker == 0xDA {
                    break;
                }
                let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
                if len < 2 {
                    break;
                }
                i += 2 + len;
            }
            (None, None)
        }
        "image/webp" => {
            // WebP VP8 lossy
            if data.len() >= 30 && &data[12..16] == b"VP8 " {
                let w = (data[26] as u32 | ((data[27] as u32) << 8)) & 0x3FFF;
                let h = (data[28] as u32 | ((data[29] as u32) << 8)) & 0x3FFF;
                (Some(w), Some(h))
            } else if data.len() >= 25 && &data[12..16] == b"VP8L" {
                // WebP VP8L lossless: 14-bit width and height packed
                let b1 = data[21] as u32;
                let b2 = data[22] as u32;
                let b3 = data[23] as u32;
                let b4 = data[24] as u32;
                let w = 1 + (((b2 & 0x3F) << 8) | b1);
                let h = 1 + (((b4 & 0xF) << 10) | (b3 << 2) | ((b2 & 0xC0) >> 6));
                (Some(w), Some(h))
            } else if data.len() >= 30 && &data[12..16] == b"VP8X" {
                // WebP VP8X extended: 24-bit canvas width-1 and height-1
                let w = 1 + (data[24] as u32 | ((data[25] as u32) << 8) | ((data[26] as u32) << 16));
                let h = 1 + (data[27] as u32 | ((data[28] as u32) << 8) | ((data[29] as u32) << 16));
                (Some(w), Some(h))
            } else {
                (None, None)
            }
        }
        "image/gif" => {
            // GIF width and height in logical screen descriptor: bytes 6..10 (little endian)
            if data.len() >= 10 {
                let w = u16::from_le_bytes([data[6], data[7]]) as u32;
                let h = u16::from_le_bytes([data[8], data[9]]) as u32;
                (Some(w), Some(h))
            } else {
                (None, None)
            }
        }
        _ => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_png_magic_byte_and_dimensions() {
        let mut png_bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        png_bytes.extend_from_slice(&[0, 0, 0, 13]);
        png_bytes.extend_from_slice(b"IHDR");
        png_bytes.extend_from_slice(&1920u32.to_be_bytes());
        png_bytes.extend_from_slice(&1080u32.to_be_bytes());
        png_bytes.extend_from_slice(&[8, 6, 0, 0, 0]);

        let payload = ImagePayload::from_bytes(png_bytes, None).unwrap();
        assert_eq!(payload.mime_type, "image/png");
        assert_eq!(payload.width, Some(1920));
        assert_eq!(payload.height, Some(1080));
        assert!(payload.to_data_uri().starts_with("data:image/png;base64,"));
        assert!(!payload.digest.is_empty());
    }

    #[test]
    fn test_gif_dimensions() {
        let mut gif_bytes = b"GIF89a".to_vec();
        gif_bytes.extend_from_slice(&800u16.to_le_bytes());
        gif_bytes.extend_from_slice(&600u16.to_le_bytes());

        let payload = ImagePayload::from_bytes(gif_bytes, None).unwrap();
        assert_eq!(payload.mime_type, "image/gif");
        assert_eq!(payload.width, Some(800));
        assert_eq!(payload.height, Some(600));
    }
}
