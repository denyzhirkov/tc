use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use rand::Rng;
use ratatui::prelude::*;
use ratatui::widgets::*;

use tc_shared::config;

use tc_shared::ChannelId;

// ── Matrix rain ─────────────────────────────────────────────────────────

const MATRIX_CHARSET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ0123456789@#$&*=<>~{}[]|:;";

struct MatrixStream {
    x: u16,
    head_y: f32,
    speed: f32,
    length: u16,
    seed: u64,
}

pub struct MatrixState {
    streams: Vec<MatrixStream>,
    tick: u64,
}

impl MatrixState {
    pub fn new() -> Self {
        Self {
            streams: Vec::new(),
            tick: 0,
        }
    }

    pub fn tick(&mut self, width: u16, height: u16, traffic_rate: f64) {
        if width == 0 || height == 0 {
            return;
        }
        self.tick += 1;
        let mut rng = rand::thread_rng();

        // Target active streams: base 8, +1.5 per KB/s, capped at 80% of width
        let target = ((8.0 + traffic_rate * 1.5) as usize).clamp(8, (width as usize) * 4 / 5);

        // Advance existing streams
        for s in &mut self.streams {
            s.head_y += s.speed;
        }

        // Remove streams that have fully scrolled off screen
        self.streams
            .retain(|s| (s.head_y as i32 - s.length as i32) < height as i32);

        // Spawn new streams to reach target count
        while self.streams.len() < target {
            self.streams.push(MatrixStream {
                x: rng.gen_range(0..width),
                head_y: -(rng.gen_range(0..height) as f32),
                speed: rng.gen_range(0.3..1.3),
                length: rng.gen_range(4..height.max(5)),
                seed: rng.gen(),
            });
        }
    }

    /// Deterministic char for a given stream+position, changing every ~200ms.
    fn char_at(&self, seed: u64, y: i32) -> char {
        let phase = self.tick / 4;
        let hash = seed
            .wrapping_mul(2654435761)
            .wrapping_add(y as u64)
            .wrapping_mul(2246822519)
            .wrapping_add(phase.wrapping_mul(6364136223846793005));
        let idx = (hash % MATRIX_CHARSET.len() as u64) as usize;
        MATRIX_CHARSET[idx] as char
    }
}

fn draw_matrix(frame: &mut Frame, state: &MatrixState) {
    let area = frame.area();
    let buf = frame.buffer_mut();
    let bg = Color::Rgb(0, 0, 0);

    // Clear to true black
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_char(' ').set_fg(bg).set_bg(bg);
        }
    }

    for stream in &state.streams {
        let head = stream.head_y as i32;
        for i in 0..stream.length as i32 {
            let y = head - i;
            if y < area.top() as i32 || y >= area.bottom() as i32 {
                continue;
            }
            if stream.x >= area.width {
                continue;
            }

            let ch = state.char_at(stream.seed, y);
            let color = if i == 0 {
                Color::White
            } else if i <= 2 {
                Color::Rgb(0, 255, 70)
            } else {
                let frac = i as f32 / stream.length as f32;
                let g = (255.0 * (1.0 - frac * 0.85)) as u8;
                Color::Rgb(0, g.max(40), 0)
            };

            buf[(stream.x, y as u16)].set_char(ch).set_fg(color).set_bg(bg);
        }
    }
}

const COMMANDS: &[(&str, &str)] = &[
    ("/config", "audio device selection"),
    ("/create", "create a new channel"),
    ("/connect", "join a channel"),
    ("/leave", "leave current channel"),
    ("/mute", "toggle mute"),
    ("/name", "set your display name"),
    ("/reconnect", "reconnect to server"),
    ("/server", "set server address"),
    ("/web", "start web UI on port"),
    ("/help", "show help"),
    ("/quit", "exit"),
];

/// Stored parameters needed to restart the voice pipeline.
#[derive(Clone)]
pub struct VoiceJoinParams {
    pub channel_id: ChannelId,
    pub udp_token: u64,
    pub voice_key: Vec<u8>,
}

/// Connection state for auto-reconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Disconnected,
    Reconnecting { attempt: u32 },
}

impl ConnectionState {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }

    pub fn is_disconnected(&self) -> bool {
        !self.is_connected()
    }
}

/// Application state for the TUI.
pub struct App {
    /// Text input buffer.
    pub input: String,
    /// Command history for REPL-style navigation.
    history: Vec<String>,
    /// Current position in history (None = new input).
    history_pos: Option<usize>,
    /// Saved input when navigating history.
    saved_input: String,
    /// Log/chat messages to display.
    pub messages: Vec<String>,
    /// Connected channel ID.
    pub channel: Option<String>,
    /// List of participants in current channel.
    pub participants: Vec<String>,
    /// Whether voice is active.
    pub voice_active: bool,
    /// Muted state.
    pub muted: Arc<AtomicBool>,
    /// Server address.
    pub server_addr: String,
    /// Voice quality: (loss_percent, tier_name).
    pub voice_quality: Option<(u8, String)>,
    /// Voice traffic: (tx_kbps, rx_kbps, total_bytes).
    pub voice_traffic: Option<(f64, f64, u64)>,
    /// Connection state (connected, disconnected, reconnecting).
    pub conn_state: ConnectionState,
    /// Whether to quit.
    pub should_quit: bool,
    /// Indices into COMMANDS that match current input.
    pub autocomplete: Vec<usize>,
    /// Selected index within autocomplete vec.
    pub autocomplete_sel: usize,
    /// Selected input device name.
    pub input_device: Option<String>,
    /// Selected output device name.
    pub output_device: Option<String>,
    /// Stored voice join params for restart.
    pub voice_join_params: Option<VoiceJoinParams>,
    /// VAD threshold (f32 stored as AtomicU32 bits for lock-free sharing with voice thread).
    pub vad_threshold: Arc<AtomicU32>,
    /// Microphone gain (f32 stored as AtomicU32 bits; 1.0 = 100%).
    pub input_gain: Arc<AtomicU32>,
    /// Incoming audio volume (f32 stored as AtomicU32 bits; 1.0 = 100%).
    pub output_vol: Arc<AtomicU32>,
    /// Display name.
    pub name: Option<String>,
    /// Channel ID to auto-rejoin after reconnect.
    pub rejoin_channel: Option<ChannelId>,
    /// Scroll offset from bottom (0 = latest messages visible).
    pub scroll_offset: usize,
    /// Names of currently speaking users.
    pub active_speakers: Vec<String>,
    /// Matrix rain easter egg.
    pub matrix_mode: bool,
    pub matrix_state: MatrixState,
    /// Dirty flag — true when UI needs redraw.
    pub dirty: bool,
}

/// Actions produced by the TUI event loop.
pub enum Action {
    /// A command was entered.
    Command(String),
    /// User wants to quit.
    Quit,
    /// No action (redraw only).
    None,
}

impl App {
    pub fn new(muted: Arc<AtomicBool>, server_addr: String) -> Self {
        Self {
            input: String::new(),
            history: Vec::new(),
            history_pos: None,
            saved_input: String::new(),
            messages: vec![
                "".into(),
                "  ████████╗  ██████╗".into(),
                "  ╚══██╔══╝ ██╔════╝".into(),
                "     ██║    ██║     ".into(),
                "     ██║    ██║     ".into(),
                "     ██║    ╚██████╗".into(),
                "     ╚═╝     ╚═════╝".into(),
                "".into(),
                "  voice chat for your terminal".into(),
                "  type /help for commands".into(),
                "".into(),
            ],
            channel: None,
            participants: Vec::new(),
            voice_active: false,
            muted,
            server_addr,
            voice_quality: None,
            voice_traffic: None,
            conn_state: ConnectionState::Disconnected,
            should_quit: false,
            autocomplete: Vec::new(),
            autocomplete_sel: 0,
            input_device: None,
            output_device: None,
            voice_join_params: None,
            vad_threshold: Arc::new(AtomicU32::new(config::VAD_RMS_THRESHOLD.to_bits())),
            input_gain: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            output_vol: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            name: None,
            rejoin_channel: None,
            scroll_offset: 0,
            active_speakers: Vec::new(),
            matrix_mode: false,
            matrix_state: MatrixState::new(),
            dirty: true, // draw on first frame
        }
    }

    fn update_autocomplete(&mut self) {
        if self.input.starts_with('/') && !self.input.contains(' ') {
            self.autocomplete = COMMANDS
                .iter()
                .enumerate()
                .filter(|(_, (cmd, _))| cmd.starts_with(&self.input))
                .map(|(i, _)| i)
                .collect();
            if self.autocomplete_sel >= self.autocomplete.len() {
                self.autocomplete_sel = self.autocomplete.len().saturating_sub(1);
            }
        } else {
            self.autocomplete.clear();
            self.autocomplete_sel = 0;
        }
    }

    pub fn add_message(&mut self, msg: String) {
        self.messages.push(msg);
        self.dirty = true;
        self.scroll_offset = 0; // snap to bottom on new message
        // Keep last N messages
        if self.messages.len() > config::MAX_MESSAGE_HISTORY {
            self.messages.drain(..self.messages.len() - config::MAX_MESSAGE_HISTORY);
        }
    }
}

/// Initialize terminal for TUI.
pub fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    Terminal::new(backend)
}

/// Restore terminal to normal state.
pub fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);
}

/// Poll for a keyboard event with a timeout. Returns an action.
pub fn poll_event(app: &mut App, timeout: Duration) -> Action {
    if event::poll(timeout).unwrap_or(false) {
        if let Ok(Event::Key(key)) = event::read() {
            if key.kind != KeyEventKind::Press {
                return Action::None;
            }

            // Ctrl+C always quits
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Action::Quit;
            }

            // Any key press marks dirty
            app.dirty = true;

            // Matrix mode: Esc exits, everything else is swallowed
            if app.matrix_mode {
                if key.code == KeyCode::Esc {
                    app.matrix_mode = false;
                }
                return Action::None;
            }

            // Ctrl+M to toggle mute
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('m') {
                return Action::Command("/mute".into());
            }

            match key.code {
                KeyCode::Enter => {
                    app.autocomplete.clear();
                    app.autocomplete_sel = 0;
                    let input = app.input.drain(..).collect::<String>();
                    let trimmed = input.trim().to_string();
                    if !trimmed.is_empty() {
                        app.history.push(trimmed.clone());
                        app.history_pos = None;
                        app.saved_input.clear();
                        return Action::Command(trimmed);
                    }
                }
                KeyCode::Up => {
                    if !app.autocomplete.is_empty() {
                        if app.autocomplete_sel > 0 {
                            app.autocomplete_sel -= 1;
                        }
                        let idx = app.autocomplete[app.autocomplete_sel];
                        app.input = COMMANDS[idx].0.to_string();
                    } else if !app.history.is_empty() {
                        match app.history_pos {
                            None => {
                                // Save current input and jump to last history entry
                                app.saved_input = app.input.clone();
                                let pos = app.history.len() - 1;
                                app.history_pos = Some(pos);
                                app.input = app.history[pos].clone();
                            }
                            Some(pos) if pos > 0 => {
                                let pos = pos - 1;
                                app.history_pos = Some(pos);
                                app.input = app.history[pos].clone();
                            }
                            _ => {}
                        }
                    }
                }
                KeyCode::Down => {
                    if !app.autocomplete.is_empty() {
                        if app.autocomplete_sel + 1 < app.autocomplete.len() {
                            app.autocomplete_sel += 1;
                        }
                        let idx = app.autocomplete[app.autocomplete_sel];
                        app.input = COMMANDS[idx].0.to_string();
                    } else if let Some(pos) = app.history_pos {
                        if pos + 1 < app.history.len() {
                            let pos = pos + 1;
                            app.history_pos = Some(pos);
                            app.input = app.history[pos].clone();
                        } else {
                            // Back to saved input
                            app.history_pos = None;
                            app.input = app.saved_input.clone();
                            app.saved_input.clear();
                        }
                    }
                }
                KeyCode::Char(c) => {
                    app.input.push(c);
                    app.history_pos = None;
                    app.update_autocomplete();
                }
                KeyCode::Backspace => {
                    app.input.pop();
                    app.update_autocomplete();
                }
                KeyCode::Tab => {
                    if !app.autocomplete.is_empty() {
                        let idx = app.autocomplete[app.autocomplete_sel];
                        app.input = format!("{} ", COMMANDS[idx].0);
                        app.autocomplete.clear();
                        app.autocomplete_sel = 0;
                    }
                }
                KeyCode::PageUp => {
                    let page = 10;
                    let max_offset = app.messages.len().saturating_sub(1);
                    app.scroll_offset = (app.scroll_offset + page).min(max_offset);
                }
                KeyCode::PageDown => {
                    let page = 10;
                    app.scroll_offset = app.scroll_offset.saturating_sub(page);
                }
                KeyCode::Home => {
                    app.scroll_offset = app.messages.len().saturating_sub(1);
                }
                KeyCode::End => {
                    app.scroll_offset = 0;
                }
                KeyCode::Esc => {
                    if !app.autocomplete.is_empty() {
                        app.autocomplete.clear();
                        app.autocomplete_sel = 0;
                    } else {
                        return Action::Quit;
                    }
                }
                _ => {}
            }
        }
    }
    Action::None
}

/// Draw the UI.
pub fn draw(frame: &mut Frame, app: &App) {
    if app.matrix_mode {
        draw_matrix(frame, &app.matrix_state);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // status bar
            Constraint::Min(5),   // messages
            Constraint::Length(3), // input
        ])
        .split(frame.area());

    // Status bar
    draw_status(frame, app, chunks[0]);

    // Messages area
    draw_messages(frame, app, chunks[1]);

    // Input area
    draw_input(frame, app, chunks[2]);

    // Autocomplete popup
    if !app.autocomplete.is_empty() {
        let height = (app.autocomplete.len() as u16).min(7);
        let width = 40.min(frame.area().width.saturating_sub(2));
        let x = chunks[2].x + 1;
        let y = chunks[2].y.saturating_sub(height);

        let popup_area = Rect::new(x, y, width, height);

        let items: Vec<ListItem> = app
            .autocomplete
            .iter()
            .enumerate()
            .map(|(i, &cmd_idx)| {
                let (cmd, desc) = COMMANDS[cmd_idx];
                let text = format!("{:<12} {}", cmd, desc);
                let style = if i == app.autocomplete_sel {
                    Style::default().fg(Color::Black).bg(Color::White)
                } else {
                    Style::default()
                };
                ListItem::new(Span::styled(text, style))
            })
            .collect();

        let list = List::new(items).block(Block::default());

        frame.render_widget(Clear, popup_area);
        frame.render_widget(list, popup_area);
    }
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let muted = app.muted.load(Ordering::Relaxed);

    let status_parts = vec![
        Span::styled(" tc ", Style::default().fg(Color::Black).bg(Color::Cyan).bold()),
        Span::raw("  "),
        match &app.conn_state {
            ConnectionState::Disconnected => Span::styled(
                " DISCONNECTED ",
                Style::default().fg(Color::White).bg(Color::Red).bold(),
            ),
            ConnectionState::Reconnecting { attempt } => Span::styled(
                format!(" RECONNECTING ({}) ", attempt),
                Style::default().fg(Color::Black).bg(Color::Yellow).bold(),
            ),
            ConnectionState::Connected => match &app.channel {
                Some(ch) => Span::styled(
                    format!(" #{} ", ch),
                    Style::default().fg(Color::Black).bg(Color::Green),
                ),
                None => Span::styled(
                    " online ",
                    Style::default().fg(Color::Black).bg(Color::DarkGray),
                ),
            },
        },
        Span::raw("  "),
        if app.voice_active {
            if muted {
                Span::styled(" MUTED ", Style::default().fg(Color::Black).bg(Color::Red))
            } else {
                Span::styled(" VOICE ON ", Style::default().fg(Color::Black).bg(Color::Green))
            }
        } else {
            Span::raw("")
        },
        Span::raw("  "),
        if let Some((loss, ref tier)) = app.voice_quality {
            let color = match tier.as_str() {
                "high" => Color::Green,
                "medium" => Color::Yellow,
                _ => Color::Red,
            };
            Span::styled(
                format!(" {}% loss ({}) ", loss, tier),
                Style::default().fg(Color::Black).bg(color),
            )
        } else {
            Span::raw("")
        },
        if let Some((tx, rx, total)) = app.voice_traffic {
            let total_str = format_bytes(total);
            Span::styled(
                format!(" ↑{:.1} ↓{:.1} KB/s ({}) ", tx, rx, total_str),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            Span::raw("")
        },
        Span::raw("  "),
        if !app.active_speakers.is_empty() {
            Span::styled(
                format!(" {} ", app.active_speakers.join(", ")),
                Style::default().fg(Color::Black).bg(Color::Magenta),
            )
        } else {
            Span::raw("")
        },
        Span::raw("  "),
        if !app.participants.is_empty() {
            Span::styled(
                format!(" {} users ", app.participants.len()),
                Style::default().fg(Color::White),
            )
        } else {
            Span::raw("")
        },
    ];

    let status = Paragraph::new(Line::from(status_parts))
        .block(Block::default().borders(Borders::BOTTOM));

    frame.render_widget(status, area);
}

fn draw_messages(frame: &mut Frame, app: &App, area: Rect) {
    let visible = area.height as usize;
    let total = app.messages.len();
    // End index (exclusive) accounting for scroll offset from bottom
    let end = total.saturating_sub(app.scroll_offset);
    let start = end.saturating_sub(visible);
    let items: Vec<ListItem> = app.messages[start..end]
        .iter()
        .map(|m| ListItem::new(Span::raw(m.as_str())))
        .collect();

    let mut block = Block::default().borders(Borders::NONE);
    if app.scroll_offset > 0 {
        block = block.title(Span::styled(
            format!(" ↑ {} more ", app.scroll_offset),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let input = Paragraph::new(app.input.as_str())
        .block(Block::default().borders(Borders::ALL).title(" > "));

    frame.render_widget(input, area);

    // Place cursor at end of input
    frame.set_cursor_position(Position::new(
        area.x + app.input.len() as u16 + 1,
        area.y + 1,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_ranges() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1 KB");
        assert_eq!(format_bytes(1536), "2 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn matrix_state_tick_no_panic() {
        let mut ms = MatrixState::new();
        ms.tick(80, 24, 0.0);
        ms.tick(80, 24, 10.0);
        assert!(!ms.streams.is_empty());
    }

    #[test]
    fn matrix_state_zero_dimensions() {
        let mut ms = MatrixState::new();
        ms.tick(0, 0, 5.0); // should not panic
        assert!(ms.streams.is_empty());
    }

    #[test]
    fn matrix_char_at_deterministic() {
        let ms = MatrixState::new();
        let c1 = ms.char_at(42, 10);
        let c2 = ms.char_at(42, 10);
        assert_eq!(c1, c2);
    }

    #[test]
    fn matrix_char_at_different_seeds() {
        let ms = MatrixState::new();
        // Different seeds should (almost certainly) give different chars
        // at different positions
        let chars: Vec<char> = (0..100).map(|i| ms.char_at(i, 0)).collect();
        let unique: std::collections::HashSet<char> = chars.into_iter().collect();
        assert!(unique.len() > 1);
    }
}
