use anyhow::Result;
use crossterm::{
    event::{KeyCode, KeyEvent, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Terminal,
};
use std::io::stdout;
use tokio::sync::broadcast;
use crate::event::{Event, PaneInfo, VoiceStatus};

pub struct TuiState {
    pub voice_status: VoiceStatus,
    pub panes: Vec<PaneInfo>,
    pub active_pane_content: String,
    pub conversation: Vec<(String, String)>,
    pub live_transcript: String,
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            voice_status: VoiceStatus::Idle,
            panes: Vec::new(),
            active_pane_content: String::new(),
            conversation: Vec::new(),
            live_transcript: String::new(),
        }
    }
}

pub struct Tui {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
    state: TuiState,
    event_tx: broadcast::Sender<Event>,
}

impl Tui {
    pub fn new(event_tx: broadcast::Sender<Event>) -> Result<Self> {
        enable_raw_mode()?;
        stdout().execute(EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout());
        let terminal = Terminal::new(backend)?;

        Ok(Self {
            terminal,
            state: TuiState::default(),
            event_tx,
        })
    }

    pub fn draw(&mut self) -> Result<()> {
        let state = &self.state;
        self.terminal.draw(|frame| {
            let size = frame.area();

            // Main layout: top bar, content, bottom bar
            let main_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Min(5),
                    Constraint::Length(3),
                ])
                .split(size);

            // Top bar
            let status_text = match &state.voice_status {
                VoiceStatus::Idle => ("Idle", Color::Gray),
                VoiceStatus::Listening => ("Listening", Color::Green),
                VoiceStatus::Thinking => ("Thinking", Color::Yellow),
                VoiceStatus::Speaking => ("Speaking", Color::Cyan),
            };
            let top_bar = Line::from(vec![
                Span::styled("  vclaw  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(
                    format!(" {} ", status_text.0),
                    Style::default().fg(Color::Black).bg(status_text.1),
                ),
            ]);
            frame.render_widget(Paragraph::new(top_bar), main_layout[0]);

            // Content: left (pane preview) | right (pane list + conversation)
            let content_layout = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(60),
                    Constraint::Percentage(40),
                ])
                .split(main_layout[1]);

            // Left: active pane preview
            let pane_preview = Paragraph::new(state.active_pane_content.as_str())
                .block(Block::default().borders(Borders::ALL).title(" Active Pane "))
                .wrap(Wrap { trim: false });
            frame.render_widget(pane_preview, content_layout[0]);

            // Right: split into pane list + conversation
            let right_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(30),
                    Constraint::Percentage(70),
                ])
                .split(content_layout[1]);

            // Pane list
            let pane_items: Vec<ListItem> = state.panes.iter().enumerate().map(|(i, p)| {
                let style = if p.active {
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(format!("[{}] {}", i + 1, p.title)).style(style)
            }).collect();
            let pane_list = List::new(pane_items)
                .block(Block::default().borders(Borders::ALL).title(" Panes "));
            frame.render_widget(pane_list, right_layout[0]);

            // Conversation log
            let conv_items: Vec<ListItem> = state.conversation.iter().map(|(role, text)| {
                let style = if role == "You" {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(format!("{}: {}", role, text)).style(style)
            }).collect();
            let conv_list = List::new(conv_items)
                .block(Block::default().borders(Borders::ALL).title(" Conversation "));
            frame.render_widget(conv_list, right_layout[1]);

            // Bottom bar: live transcript
            let transcript = Paragraph::new(format!("  > {}", state.live_transcript))
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(transcript, main_layout[2]);
        })?;

        Ok(())
    }

    pub fn update_state(&mut self, event: &Event) {
        match event {
            Event::VoiceStatus(status) => self.state.voice_status = status.clone(),
            Event::PaneListUpdated(panes) => self.state.panes = panes.clone(),
            Event::ActivePaneContent(content) => self.state.active_pane_content = content.clone(),
            Event::ConversationEntry { role, text } => {
                self.state.conversation.push((role.clone(), text.clone()));
            }
            Event::LiveTranscript(text) => self.state.live_transcript = text.clone(),
            _ => {}
        }
    }

    pub fn handle_key_event(&self, key: KeyEvent) -> Option<Event> {
        match key.code {
            KeyCode::Char('q') => Some(Event::Quit),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Event::Quit),
            KeyCode::Esc => Some(Event::Interrupt),
            _ => None,
        }
    }

    pub fn cleanup(&mut self) -> Result<()> {
        disable_raw_mode()?;
        stdout().execute(LeaveAlternateScreen)?;
        Ok(())
    }
}
