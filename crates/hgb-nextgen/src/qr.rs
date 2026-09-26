//! # Ephemeral Dev Tunnel & ASCII Mobile QR-Code
//!
//! Generates clean 2-row ANSI Unicode QR matrix (`█`, `▀`, `▄`, ` `) from URL strings:
//! - Complete self-contained pure-Rust QR Model 2 bit-matrix generator with Reed-Solomon FEC
//! - Half-block 2-row packing for high-density square terminal rendering
//! - Local LAN IP auto-detection (e.g. `http://192.168.x.x:port`)
//! - Formats authentic AGY boxed mobile preview test cards for Cockpit and CLI

use std::net::UdpSocket;

/// Auto-detect the primary non-loopback LAN IP address of this machine
pub fn detect_local_ip() -> String {
    // Attempt UDP connection to non-routable address to query OS routing table
    if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("10.255.255.255:80").is_ok() {
            if let Ok(local_addr) = socket.local_addr() {
                let ip = local_addr.ip();
                if !ip.is_loopback() && !ip.is_unspecified() {
                    return ip.to_string();
                }
            }
        }
    }
    "127.0.0.1".to_string()
}

/// Galois Field GF(2^8) arithmetic with generator poly 0x11d (x^8 + x^4 + x^3 + x^2 + 1)
struct Gf256 {
    exp: [u8; 512],
    log: [u8; 256],
}

impl Gf256 {
    fn new() -> Self {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x = 1u16;
        for i in 0..255 {
            exp[i] = x as u8;
            exp[i + 255] = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if (x & 0x100) != 0 {
                x ^= 0x11d;
            }
        }
        exp[510] = exp[0];
        exp[511] = exp[1];
        Self { exp, log }
    }

    fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            let idx = (self.log[a as usize] as usize) + (self.log[b as usize] as usize);
            self.exp[idx]
        }
    }
}

/// Generate Reed-Solomon error correction codewords
fn calculate_rs_ecc(data: &[u8], ecc_count: usize) -> Vec<u8> {
    let gf = Gf256::new();

    // Compute generator polynomial: g(x) = (x - alpha^0)(x - alpha^1)...(x - alpha^(ecc-1))
    let mut gen = vec![1u8];
    for i in 0..ecc_count {
        let root = gf.exp[i];
        let mut next_gen = vec![0u8; gen.len() + 1];
        for (j, &coeff) in gen.iter().enumerate() {
            next_gen[j] ^= gf.mul(coeff, root);
            next_gen[j + 1] ^= coeff;
        }
        gen = next_gen;
    }

    // Polynomial long division of data * x^ecc by gen
    let mut remainder = vec![0u8; ecc_count];
    for &byte in data {
        let factor = byte ^ remainder[0];
        remainder.remove(0);
        remainder.push(0);
        if factor != 0 {
            for j in 0..ecc_count {
                remainder[j] ^= gf.mul(gen[j], factor);
            }
        }
    }

    remainder
}

/// A 2D QR Bit Matrix
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrMatrix {
    pub size: usize,
    pub modules: Vec<Vec<bool>>,
}

impl QrMatrix {
    pub fn new(size: usize) -> Self {
        Self {
            size,
            modules: vec![vec![false; size]; size],
        }
    }

    /// Generate a valid QR Code matrix for a URL/text payload
    pub fn encode_url(url: &str) -> Self {
        // Choose QR Version: Version 1 (21x21, max 17 bytes at L) or Version 2 (25x25, max 32 bytes at L)
        let payload_bytes = url.as_bytes();
        let (version, size, total_data_codewords, ecc_codewords) = if payload_bytes.len() <= 14 {
            (1, 21, 19, 7) // Version 1-L: 19 data bytes, 7 ECC bytes
        } else {
            (2, 25, 34, 10) // Version 2-L: 34 data bytes, 10 ECC bytes
        };

        let mut matrix = vec![vec![None::<bool>; size]; size];

        // 1. Finder patterns at (0,0), (size-7, 0), (0, size-7)
        let finder_origins = [(0, 0), (size - 7, 0), (0, size - 7)];
        for &(ox, oy) in &finder_origins {
            for y in 0..7 {
                for x in 0..7 {
                    let is_dark = x == 0 || x == 6 || y == 0 || y == 6 || (x >= 2 && x <= 4 && y >= 2 && y <= 4);
                    matrix[oy + y][ox + x] = Some(is_dark);
                }
            }
        }

        // Separators around finders
        for &(ox, oy) in &finder_origins {
            for dy in 0..8 {
                for dx in 0..8 {
                    let px = ox as isize + dx - if ox > 0 { 1 } else { 0 };
                    let py = oy as isize + dy - if oy > 0 { 1 } else { 0 };
                    if px >= 0 && px < size as isize && py >= 0 && py < size as isize {
                        if matrix[py as usize][px as usize].is_none() {
                            matrix[py as usize][px as usize] = Some(false);
                        }
                    }
                }
            }
        }

        // 2. Alignment pattern for Version 2 at (18, 18)
        if version >= 2 {
            let ax: isize = 18;
            let ay: isize = 18;
            for dy in -2isize..=2isize {
                for dx in -2isize..=2isize {
                    let is_dark = dx.abs() == 2 || dy.abs() == 2 || (dx == 0 && dy == 0);
                    matrix[(ay + dy) as usize][(ax + dx) as usize] = Some(is_dark);
                }
            }
        }

        // 3. Timing patterns: row 6 and col 6
        for i in 8..(size - 8) {
            let is_dark = i % 2 == 0;
            if matrix[6][i].is_none() {
                matrix[6][i] = Some(is_dark);
            }
            if matrix[i][6].is_none() {
                matrix[i][6] = Some(is_dark);
            }
        }

        // 4. Dark module
        matrix[4 * version + 9][8] = Some(true);

        // 5. Reserve format information areas
        for i in 0..9 {
            if matrix[8][i].is_none() {
                matrix[8][i] = Some(false);
            }
            if matrix[i][8].is_none() {
                matrix[i][8] = Some(false);
            }
        }
        for i in (size - 8)..size {
            if matrix[8][i].is_none() {
                matrix[8][i] = Some(false);
            }
            if matrix[i][8].is_none() {
                matrix[i][8] = Some(false);
            }
        }

        // 6. Encode Data Codewords (Byte mode: 0100 + 8-bit length + data)
        let mut bitstream = Vec::new();
        // Mode indicator: Byte mode (0100)
        bitstream.extend_from_slice(&[false, true, false, false]);
        // Character count: 8 bits
        let len = payload_bytes.len().min(total_data_codewords);
        for b in (0..8).rev() {
            bitstream.push(((len >> b) & 1) == 1);
        }
        // Payload data bytes
        for &byte in &payload_bytes[..len] {
            for b in (0..8).rev() {
                bitstream.push(((byte >> b) & 1) == 1);
            }
        }
        // Terminator (up to 4 zeros)
        let max_bits = total_data_codewords * 8;
        let pad_zeros = (max_bits.saturating_sub(bitstream.len())).min(4);
        for _ in 0..pad_zeros {
            bitstream.push(false);
        }
        // Pad to byte boundary
        while bitstream.len() % 8 != 0 && bitstream.len() < max_bits {
            bitstream.push(false);
        }
        // Pad bytes (0xEC, 0x11)
        let pad_patterns = [0xECu8, 0x11u8];
        let mut pad_idx = 0;
        while bitstream.len() < max_bits {
            let pad_byte = pad_patterns[pad_idx % 2];
            for b in (0..8).rev() {
                bitstream.push(((pad_byte >> b) & 1) == 1);
            }
            pad_idx += 1;
        }

        // Convert bitstream to data codewords
        let mut data_codewords = Vec::new();
        for chunk in bitstream.chunks(8) {
            let mut byte = 0u8;
            for &bit in chunk {
                byte = (byte << 1) | (if bit { 1 } else { 0 });
            }
            data_codewords.push(byte);
        }

        // 7. Calculate Error Correction Codewords
        let ecc_codewords_vec = calculate_rs_ecc(&data_codewords, ecc_codewords);

        // Combined codewords: Data + ECC
        let mut all_bits = Vec::new();
        for &cw in data_codewords.iter().chain(ecc_codewords_vec.iter()) {
            for b in (0..8).rev() {
                all_bits.push(((cw >> b) & 1) == 1);
            }
        }

        // 8. Place Data Bits in 2-column zigzag matrix tracks
        let mut bit_idx = 0;
        let mut upward = true;
        let mut right = size - 1;

        while right > 0 {
            if right == 6 {
                right -= 1; // Skip vertical timing column
            }
            let left = right - 1;

            let row_range: Box<dyn Iterator<Item = usize>> = if upward {
                Box::new((0..size).rev())
            } else {
                Box::new(0..size)
            };

            for row in row_range {
                for &col in &[right, left] {
                    if matrix[row][col].is_none() {
                        let bit = if bit_idx < all_bits.len() {
                            all_bits[bit_idx]
                        } else {
                            false
                        };
                        bit_idx += 1;

                        // Apply Mask 0: (row + col) % 2 == 0
                        let mask = (row + col) % 2 == 0;
                        matrix[row][col] = Some(bit ^ mask);
                    }
                }
            }

            right = right.saturating_sub(2);
            upward = !upward;
        }

        // 9. Format Info (Mask 0 + Error Level L: standard format 15-bit sequence 0x7EC6)
        let format_bits: [bool; 15] = [
            true, true, true, true, true, false, true, false, false, true, false, false, false, false, false,
        ];
        // Upper left format bits
        let ul_coords = [
            (8, 0), (8, 1), (8, 2), (8, 3), (8, 4), (8, 5), (8, 7), (8, 8),
            (7, 8), (5, 8), (4, 8), (3, 8), (2, 8), (1, 8), (0, 8),
        ];
        for (i, &(r, c)) in ul_coords.iter().enumerate() {
            matrix[r][c] = Some(format_bits[i]);
        }
        // Lower left & top right format bits
        for i in 0..7 {
            matrix[size - 1 - i][8] = Some(format_bits[i]);
        }
        for i in 0..8 {
            matrix[8][size - 8 + i] = Some(format_bits[7 + i]);
        }

        // Finalize matrix into boolean grid
        let mut final_modules = vec![vec![false; size]; size];
        for y in 0..size {
            for x in 0..size {
                final_modules[y][x] = matrix[y][x].unwrap_or(false);
            }
        }

        QrMatrix {
            size,
            modules: final_modules,
        }
    }

    /// Render matrix into clean 2-row Unicode half-blocks: `█`, `▀`, `▄`, ` `
    pub fn render_half_blocks(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let border = 2;
        let total_size = self.size + border * 2;

        // Create padded grid with border
        let mut padded = vec![vec![false; total_size]; total_size];
        for y in 0..self.size {
            for x in 0..self.size {
                padded[y + border][x + border] = self.modules[y][x];
            }
        }

        // Invert for standard dark terminal background: dark module is filled foreground
        for y in (0..total_size).step_by(2) {
            let mut line = String::new();
            for x in 0..total_size {
                let top_dark = padded[y][x];
                let bot_dark = if y + 1 < total_size { padded[y + 1][x] } else { false };

                let ch = match (top_dark, bot_dark) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                };
                line.push(ch);
            }
            lines.push(line);
        }

        lines
    }
}

/// Render an authentic AGY boxed mobile preview test card with QR code
pub fn render_mobile_test_card(url: &str, port: u16, max_width: usize) -> Vec<String> {
    let qr = QrMatrix::encode_url(url);
    let qr_lines = qr.render_half_blocks();

    let qr_width = qr_lines.first().map(|l| l.chars().count()).unwrap_or(30);
    let card_width = (qr_width + 12).max(52).min(max_width.max(52));
    let mut out = Vec::new();

    // 1. Header
    let prefix = "╭─── 📱 Ephemeral Dev Tunnel & Mobile Test ";
    let dashes_cnt = card_width.saturating_sub(prefix.chars().count() + 2);
    out.push(format!("{}{}{}", prefix, "─".repeat(dashes_cnt), "╮"));

    // 2. Info lines
    let url_line = format!("│ 🌐 URL: {}", url);
    let url_pad = card_width.saturating_sub(url_line.chars().count() + 1);
    out.push(format!("{}{}{}", url_line, " ".repeat(url_pad), "│"));

    let port_line = format!("│ 🔌 Port: {} (Local LAN Tunnel)", port);
    let port_pad = card_width.saturating_sub(port_line.chars().count() + 1);
    out.push(format!("{}{}{}", port_line, " ".repeat(port_pad), "│"));

    let instr_line = "│ 📸 Scan with mobile phone camera for instant test";
    let instr_pad = card_width.saturating_sub(instr_line.chars().count() + 1);
    out.push(format!("{}{}{}", instr_line, " ".repeat(instr_pad), "│"));

    // Divider
    out.push(format!("├{}┤", "─".repeat(card_width.saturating_sub(2))));

    // 3. QR Matrix Centered
    let left_pad = card_width.saturating_sub(qr_width + 2) / 2;
    let right_pad = card_width.saturating_sub(2 + left_pad + qr_width);

    for ql in qr_lines {
        out.push(format!("│{}{}{}{}│", " ".repeat(left_pad), ql, " ".repeat(right_pad), ""));
    }

    // 4. Footer
    out.push(format!("╰{}╯", "─".repeat(card_width.saturating_sub(2))));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qr_matrix_generation_and_dimensions() {
        let qr = QrMatrix::encode_url("http://192.168.1.50:3000");
        assert!(qr.size == 21 || qr.size == 25);
        assert_eq!(qr.modules.len(), qr.size);
        assert_eq!(qr.modules[0].len(), qr.size);

        // Finder pattern top-left check: (0,0) must be dark
        assert!(qr.modules[0][0]);
        // Finder pattern center: (3,3) must be dark
        assert!(qr.modules[3][3]);
        // Finder pattern ring inner white: (1,1) must be white
        assert!(!qr.modules[1][1]);
    }

    #[test]
    fn test_half_blocks_rendering_characters() {
        let qr = QrMatrix::encode_url("http://localhost:8080");
        let lines = qr.render_half_blocks();
        assert!(!lines.is_empty());
        for line in &lines {
            for ch in line.chars() {
                assert!(ch == '█' || ch == '▀' || ch == '▄' || ch == ' ');
            }
        }
    }

    #[test]
    fn test_local_ip_detection() {
        let ip = detect_local_ip();
        assert!(!ip.is_empty());
    }

    #[test]
    fn test_render_mobile_test_card() {
        let card = render_mobile_test_card("http://192.168.0.10:3000", 3000, 70);
        assert!(!card.is_empty());
        let joined = card.join("\n");
        assert!(joined.contains("Ephemeral Dev Tunnel"));
        assert!(joined.contains("http://192.168.0.10:3000"));
        assert!(joined.contains("Scan with mobile phone"));
        assert!(joined.contains('█') || joined.contains('▀'));
    }
}
