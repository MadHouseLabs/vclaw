//! Event types and broadcast bus.
//!
//! All inter-component communication flows through the [`EventBus`], a tokio
//! broadcast channel. Each task subscribes independently, so slow consumers
//! don't block others (they receive `Lagged` errors instead).

use tokio::sync::broadcast;

/// Current state of the voice pipeline.
#[derive(Debug, Clone)]
pub enum VoiceStatus {
    Idle,
    Listening,
    Thinking,
    Speaking,
}

/// Information about a tmux pane.
#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub id: String,
    pub active: bool,
}

/// Events flowing through the broadcast bus.
#[derive(Debug, Clone)]
pub enum Event {
    /// Transcribed speech from the user, ready for brain processing.
    UserSaid(String),
    /// Voice pipeline state change (idle, listening, thinking, speaking).
    VoiceStatus(VoiceStatus),
    /// A message to add to the conversation log (shown in `vclaw ctl conversation`).
    ConversationEntry { role: String, text: String },
    /// Partial transcript update (shown while user is still speaking).
    LiveTranscript(String),
    /// Toggle push-to-talk recording (F12 key).
    VoiceToggle,
    /// Toggle mute state (Alt-M key).
    MuteToggle,
    /// Cancel current action — stops TTS, sends Ctrl+C to Claude Code.
    Interrupt,
    /// Shut down the daemon.
    Quit,
}

/// Central event bus backed by a tokio broadcast channel.
///
/// Each component subscribes independently via [`subscribe()`](EventBus::subscribe).
/// The bus has a fixed capacity; slow consumers will receive `Lagged` errors
/// and skip missed events.
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    /// Create a new event bus with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Get a clone of the sender for publishing events.
    pub fn sender(&self) -> broadcast::Sender<Event> {
        self.tx.clone()
    }

    /// Subscribe to events. Each subscriber gets its own independent receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}
