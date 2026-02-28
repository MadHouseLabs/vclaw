use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub enum VoiceStatus {
    Idle,
    Listening,
    Thinking,
    Speaking,
}

#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub id: String,
    pub title: String,
    pub size: String,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub enum Event {
    // Voice -> Brain
    UserSaid(String),
    // Brain -> TTS
    Speak(String),
    // Brain -> Tmux
    TmuxExecute(String),
    ShellInput { pane: String, text: String },
    // Tmux -> Brain (tool results)
    TmuxResult { command: String, stdout: String, stderr: String },
    PaneContent { pane: String, content: String },
    // State updates -> TUI
    VoiceStatus(VoiceStatus),
    PaneListUpdated(Vec<PaneInfo>),
    ActivePaneContent(String),
    ConversationEntry { role: String, text: String },
    LiveTranscript(String),
    // Control
    Interrupt,
    Quit,
}

pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn sender(&self) -> broadcast::Sender<Event> {
        self.tx.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}
