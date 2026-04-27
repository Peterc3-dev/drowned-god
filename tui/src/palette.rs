//! Phosphor sub-aqua palette: Drowned God + Alienware.
//! Deep ocean base, phosphor-green data, cyan live values, amber warnings,
//! salt-rust accents on damaged/idle indicators.

use ratatui::style::{Color, Modifier, Style};

pub const ABYSS: Color = Color::Rgb(4, 8, 14); // background
pub const HULL: Color = Color::Rgb(12, 24, 32); // pane background
pub const HULL_HI: Color = Color::Rgb(20, 40, 56); // pane border highlight
pub const PHOSPHOR: Color = Color::Rgb(80, 255, 140); // primary text
pub const PHOSPHOR_DIM: Color = Color::Rgb(40, 140, 80);
pub const SONAR_CYAN: Color = Color::Rgb(80, 220, 240); // live values, ping
pub const SONAR_DIM: Color = Color::Rgb(20, 100, 130);
pub const AMBER: Color = Color::Rgb(255, 180, 60); // warnings
pub const SALT_RUST: Color = Color::Rgb(180, 90, 40); // damaged / idle
pub const KRAKEN_INK: Color = Color::Rgb(60, 0, 90); // accent
pub const BONE: Color = Color::Rgb(220, 220, 200); // headers, callouts

pub fn s_pane_border() -> Style {
    Style::default().fg(HULL_HI).bg(ABYSS)
}

pub fn s_title() -> Style {
    Style::default().fg(BONE).bg(ABYSS).add_modifier(Modifier::BOLD)
}

pub fn s_text() -> Style {
    Style::default().fg(PHOSPHOR).bg(ABYSS)
}

pub fn s_text_dim() -> Style {
    Style::default().fg(PHOSPHOR_DIM).bg(ABYSS)
}

pub fn s_live() -> Style {
    Style::default().fg(SONAR_CYAN).bg(ABYSS).add_modifier(Modifier::BOLD)
}

pub fn s_warn() -> Style {
    Style::default().fg(AMBER).bg(ABYSS).add_modifier(Modifier::BOLD)
}

pub fn s_idle() -> Style {
    Style::default().fg(SALT_RUST).bg(ABYSS)
}

pub fn s_accent() -> Style {
    Style::default().fg(KRAKEN_INK).bg(ABYSS)
}
