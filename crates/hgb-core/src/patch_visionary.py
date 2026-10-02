import os

filepath = "visionary.rs"
with open(filepath, "r") as f:
    content = f.read()

target = """    fn spawn_hud(&self) -> Result<(), Box<dyn Error>> {
        println!("Spawning Native Ghost Overlay HUD (Transparent: {})", self.transparent);
        Ok(())
    }"""

replacement = """    fn spawn_hud(&self) -> Result<(), Box<dyn Error>> {
        use std::time::Duration;
        use ratatui::{backend::CrosstermBackend, Terminal, widgets::{Block, Borders, Paragraph}};
        use crossterm::{terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}, execute};

        if let Ok(_) = enable_raw_mode() {
            let mut stdout = std::io::stdout();
            if execute!(stdout, EnterAlternateScreen).is_ok() {
                let backend = CrosstermBackend::new(stdout);
                if let Ok(mut terminal) = Terminal::new(backend) {
                    let _ = terminal.draw(|f| {
                        let size = f.size();
                        let block = Block::default().title("Ghost Overlay HUD").borders(Borders::ALL);
                        let p = Paragraph::new(format!("Transparent: {}", self.transparent)).block(block);
                        f.render_widget(p, size);
                    });
                    std::thread::sleep(Duration::from_secs(2));
                    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
                }
            }
            let _ = disable_raw_mode();
        }
        println!("Spawning Native Ghost Overlay HUD (Transparent: {})", self.transparent);
        Ok(())
    }"""

with open(filepath, "w") as f:
    f.write(content.replace(target, replacement))

