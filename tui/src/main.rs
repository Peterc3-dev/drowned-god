//! drowned-god — Alienware-mothership cockpit TUI for the local-LLM agent rig.
//!
//! Layout (cockpit HUD):
//!
//!   ┌─ TL corner ──┬──────── HEADER (rig name, target, mode, clock) ──────┬─ TR corner ──┐
//!   │                                                                                    │
//!   │  ┌─ MODELS ───┐  ┌─ CHAT ──────────────────────────┐  ┌─ GAUGES ───┐               │
//!   │  │ qwen3-8b   │  │ > query                         │  │ TG     tps │               │
//!   │  │ qwen3-1.7b │  │ < response stream …             │  │ PP     tps │               │
//!   │  │ flm/qwen3  │  │                                 │  │ ACC    %   │               │
//!   │  │ ...        │  │                                 │  │ MEM    GB  │               │
//!   │  └────────────┘  │                                 │  │ VRAM   GB  │               │
//!   │                  │                                 │  │ CPU    %   │               │
//!   │  ┌─ TOOL LOG ─┐  │                                 │  └────────────┘               │
//!   │  │            │  │                                 │  ┌─ MEMORY ───┐               │
//!   │  │            │  │                                 │  │ episodic   │               │
//!   │  │            │  │                                 │  │ entries    │               │
//!   │  └────────────┘  └─────────────────────────────────┘  └────────────┘               │
//!   │                                                                                    │
//!   ├─ BL corner ──┴────────── FOOTER (hints, status line) ──────────────┴─ BR corner ──┤
//!
//! This file is the application skeleton + render loop. Subsystems live in
//! their own modules so the file stays under 300 lines.

mod chat;
mod corners;
mod palette;
mod telemetry;

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chat::ChatMessage;
use palette as p;
use telemetry::Telemetry;

#[derive(Default)]
struct App {
    tick: u64,
    chat: Vec<String>,
    tool_log: Vec<String>,
    selected_model: usize,
    input: String,
    quit: bool,
    /// Conversation history sent to llama-server. Distinct from `chat` (which
    /// is the human-readable transcript pane).
    history: Vec<ChatMessage>,
    /// In-flight assistant response. When `Some`, we don't accept new sends.
    pending: Option<chat::ChatRx>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let llama_url = std::env::var("DG_LLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let flm_url = std::env::var("DG_FLM_URL").unwrap_or_else(|_| "http://127.0.0.1:52625".into());

    let telem = Arc::new(Mutex::new(Telemetry::default()));
    telemetry::spawn(telem.clone(), llama_url.clone(), flm_url.clone());

    let http_client = chat::build_client();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::default();
    app.chat.push("[boot] Drowned God cockpit online.".into());
    app.tool_log.push("[boot] telemetry streaming.".into());

    let res = run_app(&mut terminal, &mut app, &telem, http_client, llama_url).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    res
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    telem: &Arc<Mutex<Telemetry>>,
    http_client: Arc<reqwest::Client>,
    llama_url: String,
) -> Result<()> {
    loop {
        let snapshot = telem.lock().map(|t| t.clone()).unwrap_or_default();
        // Drain pending chat reply if it arrived.
        if let Some(rx) = app.pending.as_mut() {
            match rx.try_recv() {
                Ok(Ok(content)) => {
                    let trimmed = content.trim().to_string();
                    // Replace the "< …" placeholder we pushed when the request
                    // was sent. Defensive: only pop if the last line is the
                    // placeholder we expect.
                    if app.chat.last().map(|s| s.as_str()) == Some("< …") {
                        app.chat.pop();
                    }
                    app.chat.push(format!("< {}", trimmed));
                    app.history.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: trimmed,
                    });
                    // Sliding-window history truncation — keep last MAX_TURNS
                    // user/assistant pairs so we don't exceed llama-server ctx
                    // on long sessions.
                    const MAX_TURNS: usize = 24;
                    if app.history.len() > MAX_TURNS * 2 {
                        let drop_n = app.history.len() - MAX_TURNS * 2;
                        app.history.drain(..drop_n);
                    }
                    app.pending = None;
                }
                Ok(Err(e)) => {
                    if app.chat.last().map(|s| s.as_str()) == Some("< …") {
                        app.chat.pop();
                    }
                    app.chat.push(format!("[err] {}", e));
                    // Roll back the unsent user turn so retry works
                    if matches!(app.history.last(), Some(m) if m.role == "user") {
                        app.history.pop();
                    }
                    app.pending = None;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    // still waiting
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    if app.chat.last().map(|s| s.as_str()) == Some("< …") {
                        app.chat.pop();
                    }
                    app.chat.push("[err] chat task dropped without reply".into());
                    if matches!(app.history.last(), Some(m) if m.role == "user") {
                        app.history.pop();
                    }
                    app.pending = None;
                }
            }
        }
        terminal.draw(|f| ui(f, app, &snapshot))?;
        if event::poll(Duration::from_millis(120))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
                    KeyCode::Up => {
                        if app.selected_model > 0 {
                            app.selected_model -= 1;
                        }
                    }
                    KeyCode::Down => {
                        // bound by total models known (local + NPU); polled snapshot
                        let max = (snapshot.flm_models.len() + if snapshot.llama_alive { 1 } else { 0 }).saturating_sub(1);
                        if app.selected_model < max {
                            app.selected_model += 1;
                        }
                    }
                    KeyCode::Char(c) => app.input.push(c),
                    KeyCode::Backspace => {
                        app.input.pop();
                    }
                    KeyCode::Enter => {
                        if !app.input.is_empty() && app.pending.is_none() && snapshot.llama_alive {
                            let msg = std::mem::take(&mut app.input);
                            app.chat.push(format!("> {}", msg));
                            app.history.push(ChatMessage {
                                role: "user".to_string(),
                                content: msg,
                            });
                            app.chat.push("< …".into());
                            app.pending = Some(chat::spawn_chat_request(
                                http_client.clone(),
                                llama_url.clone(),
                                snapshot.llama_model.clone(),
                                app.history.clone(),
                            ));
                        } else if app.pending.is_some() {
                            // ignore Enter while waiting
                        } else if !snapshot.llama_alive {
                            app.chat.push("[err] llama-server unreachable".into());
                        }
                    }
                    _ => {}
                }
            }
        }
        app.tick = app.tick.wrapping_add(1);
        if app.quit {
            break;
        }
    }
    Ok(())
}

fn ui(f: &mut Frame, app: &App, t: &Telemetry) {
    let area = f.area();
    // Outer 3-row split: header strip / body / footer strip
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(corners::CORNER_H + 1), // header w/ corners
            Constraint::Min(10),
            Constraint::Length(corners::CORNER_H + 1), // footer w/ corners
        ])
        .split(area);

    draw_header(f, outer[0], app, t);
    draw_body(f, outer[1], app, t);
    draw_footer(f, outer[2], app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App, t: &Telemetry) {
    // Split: TL corner | center HUD | TR corner
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(corners::CORNER_W),
            Constraint::Min(20),
            Constraint::Length(corners::CORNER_W),
        ])
        .split(area);

    // Corner TL
    let tl = corners::frame_tl(app.tick);
    let tl_lines: Vec<Line> = tl.iter().map(|s| Line::from(Span::styled(*s, p::s_pane_border()))).collect();
    f.render_widget(Paragraph::new(tl_lines), cols[0]);

    // Center: rig name + target model + clock
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let header_lines = vec![
        Line::from(vec![
            Span::styled("D R O W N E D   G O D ", p::s_title()),
            Span::styled("· cockpit · ", p::s_text_dim()),
            Span::styled(format!("rig: raz-gpd4 "), p::s_text()),
            Span::styled(format!("· {}", now), p::s_text_dim()),
        ]),
        Line::from(vec![
            Span::styled("target: ", p::s_text_dim()),
            Span::styled(if t.llama_alive { &t.llama_model } else { "(offline)" }, p::s_live()),
            Span::styled("   draft: ", p::s_text_dim()),
            Span::styled("Qwen3-1.7B-Q4_K_M", p::s_text()),
            Span::styled("   NPU: ", p::s_text_dim()),
            Span::styled(
                if t.flm_alive { format!("{} models", t.flm_models.len()) } else { "offline".into() },
                if t.flm_alive { p::s_live() } else { p::s_idle() },
            ),
        ]),
        Line::from(vec![
            Span::styled("mode: ", p::s_text_dim()),
            Span::styled("agent-brick", p::s_warn()),
            Span::styled("   ctx: ", p::s_text_dim()),
            Span::styled(format!("{}", t.llama_n_ctx), p::s_text()),
        ]),
        Line::from(vec![
            Span::styled("─".repeat(area.width.saturating_sub(corners::CORNER_W * 2) as usize), p::s_pane_border()),
        ]),
    ];
    f.render_widget(Paragraph::new(header_lines), cols[1]);

    // Corner TR
    let tr = corners::frame_tr(app.tick);
    let tr_lines: Vec<Line> = tr.iter().map(|s| Line::from(Span::styled(*s, p::s_pane_border()))).collect();
    f.render_widget(Paragraph::new(tr_lines), cols[2]);
}

fn draw_body(f: &mut Frame, area: Rect, app: &App, t: &Telemetry) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(20), // models + tool log column
            Constraint::Min(40),    // chat
            Constraint::Length(28), // gauges + memory
        ])
        .split(area);

    // Left column: models / tool log
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(cols[0]);
    draw_models(f, left[0], app, t);
    draw_tool_log(f, left[1], app);

    // Center: chat
    draw_chat(f, cols[1], app);

    // Right column: gauges / memory
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(14), Constraint::Min(5)])
        .split(cols[2]);
    draw_gauges(f, right[0], t);
    draw_memory(f, right[1]);
}

fn draw_models(f: &mut Frame, area: Rect, _app: &App, t: &Telemetry) {
    let mut items: Vec<ListItem> = Vec::new();
    let local: Vec<&str> = if t.llama_alive { vec![t.llama_model.as_str()] } else { vec![] };
    for m in local {
        items.push(ListItem::new(Span::styled(format!("● {}", m), p::s_live())));
    }
    for m in &t.flm_models {
        items.push(ListItem::new(Span::styled(format!("∘ NPU/{}", m), p::s_text_dim())));
    }
    let block = Block::default().title(Span::styled(" MODELS ", p::s_title())).borders(Borders::ALL).border_style(p::s_pane_border());
    let list = List::new(items).block(block).highlight_style(p::s_warn());
    f.render_widget(list, area);
}

fn draw_chat(f: &mut Frame, area: Rect, app: &App) {
    let inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);

    let lines: Vec<Line> = app.chat.iter().rev().take(inner[0].height as usize).rev().map(|s| {
        let style = if s.starts_with('>') { p::s_live() } else if s.starts_with("[boot]") { p::s_text_dim() } else { p::s_text() };
        Line::from(Span::styled(s.as_str(), style))
    }).collect();
    let block = Block::default().title(Span::styled(" CHAT ", p::s_title())).borders(Borders::ALL).border_style(p::s_pane_border());
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), inner[0]);

    let prompt = format!("> {}_", app.input);
    let input_block = Block::default().borders(Borders::ALL).border_style(p::s_pane_border());
    f.render_widget(Paragraph::new(Span::styled(prompt, p::s_live())).block(input_block), inner[1]);
}

fn draw_tool_log(f: &mut Frame, area: Rect, app: &App) {
    let lines: Vec<Line> = app.tool_log.iter().rev().take(area.height.saturating_sub(2) as usize).rev().map(|s| {
        Line::from(Span::styled(s.as_str(), p::s_text_dim()))
    }).collect();
    let block = Block::default().title(Span::styled(" TOOL LOG ", p::s_title())).borders(Borders::ALL).border_style(p::s_pane_border());
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_gauges(f: &mut Frame, area: Rect, t: &Telemetry) {
    let block = Block::default().title(Span::styled(" GAUGES ", p::s_title())).borders(Borders::ALL).border_style(p::s_pane_border());
    f.render_widget(block.clone(), area);
    let inner = block.inner(area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // TG
            Constraint::Length(2), // PP
            Constraint::Length(2), // ACCEPT
            Constraint::Length(2), // MEM
            Constraint::Length(2), // VRAM
            Constraint::Length(2), // CPU
        ])
        .split(inner);

    let mem_pct = if t.mem_total_mb > 0 { (t.mem_used_mb * 100 / t.mem_total_mb) as u16 } else { 0 };
    let vram_pct = if t.vram_total_mb > 0 { (t.vram_used_mb * 100 / t.vram_total_mb) as u16 } else { 0 };
    let acc_pct = (t.last_accept_rate * 100.0).clamp(0.0, 100.0) as u16;
    let cpu_pct = t.cpu_pct.clamp(0.0, 100.0) as u16;

    f.render_widget(gauge("TG  t/s", (t.last_tg_tps.min(50.0) * 2.0) as u16, &format!("{:.1}", t.last_tg_tps)), rows[0]);
    f.render_widget(gauge("PP  t/s", (t.last_pp_tps.min(500.0) / 5.0) as u16, &format!("{:.0}", t.last_pp_tps)), rows[1]);
    f.render_widget(gauge("ACCEPT", acc_pct, &format!("{}%", acc_pct)), rows[2]);
    f.render_widget(gauge("MEM   ", mem_pct, &format!("{}/{}G", t.mem_used_mb / 1024, t.mem_total_mb / 1024)), rows[3]);
    f.render_widget(gauge("VRAM  ", vram_pct, &format!("{}/{}G", t.vram_used_mb / 1024, t.vram_total_mb / 1024)), rows[4]);
    f.render_widget(gauge("CPU   ", cpu_pct, &format!("{}%", cpu_pct)), rows[5]);
}

fn gauge<'a>(label: &'a str, pct: u16, value: &'a str) -> Gauge<'a> {
    let style = if pct > 85 { p::s_warn() } else if pct > 60 { p::s_live() } else { p::s_text() };
    Gauge::default()
        .gauge_style(style)
        .ratio((pct as f64 / 100.0).clamp(0.0, 1.0))
        .label(format!("{} {}", label, value))
}

fn draw_memory(f: &mut Frame, area: Rect) {
    let block = Block::default().title(Span::styled(" EPISODIC MEMORY ", p::s_title())).borders(Borders::ALL).border_style(p::s_pane_border());
    let lines = vec![
        Line::from(Span::styled("(TODO: wire CIN/INDEX.md", p::s_text_dim())),
        Line::from(Span::styled("       + memory/ folder)", p::s_text_dim())),
    ];
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(corners::CORNER_W),
            Constraint::Min(20),
            Constraint::Length(corners::CORNER_W),
        ])
        .split(area);

    let bl = corners::frame_bl(app.tick);
    let bl_lines: Vec<Line> = bl.iter().map(|s| Line::from(Span::styled(*s, p::s_pane_border()))).collect();
    f.render_widget(Paragraph::new(bl_lines), cols[0]);

    let footer_lines = vec![
        Line::from(Span::styled("─".repeat(area.width.saturating_sub(corners::CORNER_W * 2) as usize), p::s_pane_border())),
        Line::from(vec![
            Span::styled(" [↑↓] model  ", p::s_text_dim()),
            Span::styled(" [enter] send  ", p::s_text_dim()),
            Span::styled(" [esc/q] quit  ", p::s_text_dim()),
        ]),
        Line::from(vec![
            Span::styled(" sonar: ", p::s_text_dim()),
            Span::styled(sonar_pulse(app.tick), p::s_live()),
        ]),
        Line::from(Span::styled("·".repeat(area.width.saturating_sub(corners::CORNER_W * 2) as usize), p::s_text_dim())),
    ];
    f.render_widget(Paragraph::new(footer_lines), cols[1]);

    let br = corners::frame_br(app.tick);
    let br_lines: Vec<Line> = br.iter().map(|s| Line::from(Span::styled(*s, p::s_pane_border()))).collect();
    f.render_widget(Paragraph::new(br_lines), cols[2]);
}

fn sonar_pulse(tick: u64) -> &'static str {
    match (tick / 4) % 8 {
        0 => "◉      ",
        1 => "·◉     ",
        2 => "··◉    ",
        3 => "···◉   ",
        4 => "····◉  ",
        5 => "·····◉ ",
        6 => "······◉",
        _ => "·······",
    }
}
