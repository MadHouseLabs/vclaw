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
    pub active: bool,
}

#[derive(Debug, Clone)]
pub enum Event {
    // Voice -> Brain
    UserSaid(String),
    // State updates
    VoiceStatus(VoiceStatus),
    ConversationEntry { role: String, text: String },
    LiveTranscript(String),
    // Control
    VoiceToggle,
    MuteToggle,
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
