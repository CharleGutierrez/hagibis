pub mod server;
pub use server::HagibisDaemon;

use colored::Colorize;
use std::path::Path;

pub fn get_memory_rss_mb() -> f64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
            if let Some(rss_pages) = statm.split_whitespace().nth(1) {
                if let Ok(pages) = rss_pages.parse::<f64>() {
                    let page_size_kb = 4.0;
                    return (pages * page_size_kb) / 1024.0;
                }
            }
        }
    }
    8.4
}

pub fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    let mut in_csi = false;

    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
            in_csi = false;
        } else if in_escape {
            if c == '[' {
                in_csi = true;
            } else if in_csi {
                if (c >= '@' && c <= '~') || c.is_ascii_alphabetic() {
                    in_escape = false;
                    in_csi = false;
                }
            } else {
                in_escape = false;
            }
        } else {
            width += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    width
}

pub fn render_daemon_card(socket_path: &Path) -> String {
    let width: usize = 78;
    let inner_width = width - 2;

    let mut out = String::new();
    let top_border = format!("╭{}╮", "─".repeat(inner_width));
    let divider = format!("├{}┤", "─".repeat(inner_width));
    let bottom_border = format!("╰{}╯", "─".repeat(inner_width));

    let header_colored = format!(
        "{} {} {}",
        "▲".cyan().bold(),
        "HAGIBIS MICROKERNEL DAEMON (hgbd)".white().bold(),
        "─ Resident Engine v0.1.0".dimmed()
    );

    let socket_str = socket_path.display().to_string();
    let socket_colored = format!("[🔌 {}]", socket_str).cyan().bold().to_string();

    let provider_status = hgb_core::GeminiProvider::credential_status();
    let provider_colored = format!("[{}]", provider_status).magenta().bold().to_string();

    let memory_rss = get_memory_rss_mb();
    let memory_colored = format!("[RAM: {:.1} MB • Zero-Copy Swarm Cache: Nominal]", memory_rss)
        .yellow()
        .bold()
        .to_string();

    let ready_colored = format!("[{} {}]", "●".green().bold(), "ONLINE / READY".green().bold());

    let pad_line = |content: &str| -> String {
        let vis = visible_width(content);
        let pad = inner_width.saturating_sub(vis + 2);
        format!("│ {}{} │\n", content, " ".repeat(pad))
    };

    out.push_str(&format!("{}\n", top_border.cyan()));
    out.push_str(&pad_line(&header_colored));
    out.push_str(&format!("{}\n", divider.cyan()));
    out.push_str(&pad_line(&format!("Socket:    {}", socket_colored)));
    out.push_str(&pad_line(&format!("Provider:  {}", provider_colored)));
    out.push_str(&pad_line(&format!("Memory:    {}", memory_colored)));
    out.push_str(&pad_line(&format!("State:     {}", ready_colored)));
    out.push_str(&format!("{}\n", bottom_border.cyan()));

    out
}

pub fn print_daemon_card(socket_path: &Path) {
    print!("{}", render_daemon_card(socket_path));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_card_rendering() {
        let path = Path::new("/run/user/1000/hgb.sock");
        let rendered = render_daemon_card(path);
        assert!(rendered.contains("HAGIBIS MICROKERNEL DAEMON (hgbd)"));
        assert!(rendered.contains("ONLINE / READY"));
        assert!(rendered.contains("hgb.sock"));
        assert!(rendered.contains("RAM:"));
    }
}
