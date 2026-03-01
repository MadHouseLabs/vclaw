use anyhow::Result;
use futures::StreamExt;
use reqwest::Client;
use reqwest_eventsource::{Event as SseEvent, EventSource};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub control_type: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Message {
    pub role: String,
    pub content: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Debug)]
pub enum StreamEvent {
    ContentBlockStart {
        index: usize,
        block_type: String,
        id: Option<String>,
        name: Option<String>,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    InputJsonDelta {
        index: usize,
        partial_json: String,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        stop_reason: String,
    },
    Done,
}

pub fn is_complex_request(user_said: &str) -> bool {
    let lower = user_said.to_lowercase();
    let complex_keywords = [
        "explain", "debug", "why", "how does", "refactor",
        "review", "analyze", "architecture", "design", "compare",
        "difference between", "what is", "help me understand",
    ];
    complex_keywords.iter().any(|k| lower.contains(k))
}

pub fn build_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "shell_input".into(),
            description: "Type a prompt or response into Claude Code's tmux pane and press Enter to submit it. ALWAYS use this tool when the user gives a command — call it in the SAME response as speak, never speak without also typing.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pane": {
                        "type": "string",
                        "description": "Target pane ID (e.g., '%0', '%1')"
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type into Claude Code and submit. Enter is pressed automatically."
                    }
                },
                "required": ["pane", "text"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "speak".into(),
            description: "Speak a message aloud to the user via text-to-speech. Use this to communicate results, confirmations, or ask questions.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The message to speak aloud"
                    }
                },
                "required": ["message"]
            }),
            cache_control: None,
        },
    ]
}

/// Find the most recent Claude Code JSONL transcript for a project.
pub fn find_latest_jsonl(project_dir: &str) -> Option<std::path::PathBuf> {
    let projects_dir = dirs::home_dir()?.join(".claude/projects").join(project_dir);
    if !projects_dir.exists() {
        return None;
    }

    let mut jsonl_files: Vec<_> = std::fs::read_dir(&projects_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "jsonl").unwrap_or(false))
        .collect();
    jsonl_files.sort_by_key(|e| std::cmp::Reverse(e.metadata().ok().and_then(|m| m.modified().ok())));

    jsonl_files.first().map(|f| f.path())
}

/// Claude Code's current state, derived from the JSONL transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeCodeState {
    /// Last entry is assistant tool_use with no tool_result — waiting for permission
    WaitingForPermission { tool_name: String },
    /// Last entry is assistant text — finished, waiting for new prompt
    Idle,
    /// Still working (progress entries, or tool_result followed by more work)
    Working,
    /// No entries / unknown
    Unknown,
}

/// Parse JSONL lines into human-readable conversation entries.
/// Also tracks the last significant entry type for state detection.
fn parse_jsonl_entries(content: &str) -> (Vec<String>, ClaudeCodeState) {
    let mut entries = Vec::new();
    // Track last significant event for state detection
    let mut last_event: Option<(&str, &str)> = None; // (type, role)
    let mut last_tool_name = String::new();
    let mut last_assistant_had_text = false;

    for line in content.lines() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let role = v.pointer("/message/role").and_then(|r| r.as_str()).unwrap_or("");

        match (msg_type, role) {
            ("user", "user") => {
                // Check if this is a tool_result
                let is_tool_result = v.pointer("/message/content")
                    .and_then(|c| c.as_array())
                    .map(|arr| arr.iter().any(|item| {
                        item.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                    }))
                    .unwrap_or(false);

                if is_tool_result {
                    last_event = Some(("tool_result", "user"));
                    last_assistant_had_text = false;
                } else {
                    last_event = Some(("user", "user"));
                    last_assistant_had_text = false;
                }

                if let Some(content) = v.pointer("/message/content") {
                    if let Some(text) = content.as_str() {
                        if !text.is_empty() && text.len() < 500 {
                            entries.push(format!("User: {}", text));
                        }
                    } else if let Some(arr) = content.as_array() {
                        for item in arr {
                            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                    if !text.is_empty() && text.len() < 500 {
                                        entries.push(format!("User: {}", text));
                                    } else if text.len() >= 500 {
                                        entries.push(format!("User: {}...", &text[..200]));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ("assistant", "assistant") => {
                let stop = v.pointer("/message/stop_reason")
                    .or_else(|| v.get("stop_reason"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");

                last_assistant_had_text = false;

                if let Some(arr) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                    for item in arr {
                        let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match item_type {
                            "text" => {
                                last_assistant_had_text = true;
                                last_event = Some(("assistant_text", "assistant"));
                                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                    if !text.is_empty() {
                                        let truncated = if text.len() > 300 {
                                            format!("{}...", &text[..300])
                                        } else {
                                            text.to_string()
                                        };
                                        entries.push(format!("Claude Code: {}", truncated));
                                    }
                                }
                            }
                            "tool_use" => {
                                last_tool_name = item.get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                last_event = Some(("tool_use", "assistant"));
                                last_assistant_had_text = false;
                                entries.push(format!("Claude Code: [using tool: {}]", last_tool_name));
                            }
                            // Skip thinking blocks — internal reasoning, not a state change
                            "thinking" => {}
                            _ => {}
                        }
                    }
                }

                // Use stop_reason for definitive Idle detection
                if stop == "end_turn" && last_assistant_had_text {
                    last_event = Some(("end_turn", "assistant"));
                }
            }
            // Progress and file-history-snapshot are transient metadata —
            // do NOT override last_event, they don't change conversation state
            ("progress", _) | ("file-history-snapshot", _) => {}
            _ => {}
        }
    }

    // Determine state from last significant conversation event
    let state = match last_event {
        Some(("tool_use", "assistant")) => {
            ClaudeCodeState::WaitingForPermission { tool_name: last_tool_name }
        }
        Some(("end_turn", "assistant")) => ClaudeCodeState::Idle,
        Some(("assistant_text", "assistant")) if last_assistant_had_text => {
            ClaudeCodeState::Idle
        }
        Some(("tool_result", "user")) => ClaudeCodeState::Working,
        Some(("user", "user")) => ClaudeCodeState::Working,
        _ => ClaudeCodeState::Unknown,
    };

    (entries, state)
}

/// Read the most recent Claude Code JSONL transcript and extract a summary
/// of recent conversation. Returns (summary_text, file_path, file_size).
pub fn load_claude_code_history(project_dir: &str) -> (String, Option<std::path::PathBuf>, u64) {
    let path = match find_latest_jsonl(project_dir) {
        Some(p) => p,
        None => return (String::new(), None, 0),
    };

    tracing::info!("Loading Claude Code history from: {:?}", path);

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return (String::new(), None, 0),
    };
    let file_size = file.metadata().ok().map(|m| m.len()).unwrap_or(0);

    // Read last ~100KB
    let content = {
        let read_from = if file_size > 100_000 { file_size - 100_000 } else { 0 };
        use std::io::{Read, Seek, SeekFrom};
        let mut file = file;
        let _ = file.seek(SeekFrom::Start(read_from));
        let mut buf = String::new();
        let _ = file.read_to_string(&mut buf);
        if read_from > 0 {
            if let Some(pos) = buf.find('\n') {
                buf = buf[pos + 1..].to_string();
            }
        }
        buf
    };

    let (entries, _state) = parse_jsonl_entries(&content);

    // Keep last 30 entries
    let recent: Vec<&String> = if entries.len() > 30 {
        entries[entries.len() - 30..].iter().collect()
    } else {
        entries.iter().collect()
    };

    let summary = if recent.is_empty() {
        String::new()
    } else {
        recent.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
    };

    (summary, Some(path), file_size)
}

/// Poll for new JSONL entries since `last_offset`.
/// `last_path` tracks which file we were reading — if the latest file changes
/// (new Claude Code session), we reset to offset 0 so we don't miss the start.
/// Returns (new_entries_text, new_path, new_offset, claude_code_state).
pub fn poll_claude_code_history(
    project_dir: &str,
    last_offset: u64,
    last_path: Option<&std::path::Path>,
) -> (String, Option<std::path::PathBuf>, u64, ClaudeCodeState) {
    let path = match find_latest_jsonl(project_dir) {
        Some(p) => p,
        None => return (String::new(), None, last_offset, ClaudeCodeState::Unknown),
    };

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return (String::new(), Some(path), last_offset, ClaudeCodeState::Unknown),
    };
    let file_size = file.metadata().ok().map(|m| m.len()).unwrap_or(0);

    // If the file changed (new Claude Code session), reset offset to read from start
    let effective_offset = if last_path.map_or(true, |lp| lp != path) {
        tracing::info!("JSONL file changed to {:?}, resetting offset (was {})", path, last_offset);
        0
    } else {
        last_offset
    };

    if file_size <= effective_offset {
        return (String::new(), Some(path), effective_offset, ClaudeCodeState::Unknown);
    }

    use std::io::{Read, Seek, SeekFrom};
    let mut file = file;
    let _ = file.seek(SeekFrom::Start(effective_offset));
    let mut buf = String::new();
    let _ = file.read_to_string(&mut buf);

    let (entries, state) = parse_jsonl_entries(&buf);

    let summary = if entries.is_empty() {
        String::new()
    } else {
        entries.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
    };

    (summary, Some(path), file_size, state)
}

pub fn build_system_prompt(claude_md: &str, claude_code_history: &str) -> String {
    let project_context = if claude_md.is_empty() && claude_code_history.is_empty() {
        String::new()
    } else {
        let mut ctx = String::new();
        if !claude_md.is_empty() {
            ctx.push_str(&format!("\n\nProject context from CLAUDE.md:\n{}", claude_md));
        }
        if !claude_code_history.is_empty() {
            ctx.push_str(&format!("\n\nRecent Claude Code conversation (use this to understand what the user has been working on):\n{}", claude_code_history));
        }
        ctx
    };
    let base_prompt = r#"You are vclaw, a voice interface for Claude Code. You hear the user speak and translate their voice commands into well-crafted prompts that you type into Claude Code running in the terminal.

=== MANDATORY RULES (never violate these) ===

RULE 1 — ALWAYS TYPE + SPEAK: When the user gives a command or instruction, you MUST call BOTH shell_input AND speak in the SAME response. No exceptions. If you only speak without calling shell_input, the user's command is lost — nothing happens in the terminal. This is the #1 most important rule.

RULE 2 — SHELL_INPUT SUBMITS AUTOMATICALLY: The shell_input tool always presses Enter after typing. There is no enter parameter. Just provide pane and text — it will be typed and submitted.

RULE 3 — NEVER SEND EMPTY TEXT: The text field in shell_input must contain the actual prompt or response. Never send blank or whitespace-only text.

Self-check before every response: "Did the user give a command? If yes, am I calling shell_input? If not, I am violating Rule 1."

=== Your job ===

1. LISTEN to what the user says
2. ENRICH their voice command with context from your conversation memory (what they've been working on, past requests, known file paths, bugs, etc.)
3. GENERATE a detailed prompt and TYPE it into Claude Code using shell_input — IMMEDIATELY, in the SAME response as your speak
4. MONITOR Claude Code's output — handle permission prompts (approve them), report completion or errors via speak
5. NEVER try to run code, edit files, or do dev work yourself — that's Claude Code's job. You are the voice-to-prompt translator.

=== Generating prompts for Claude Code ===

- Take the user's brief voice instruction and expand it into a clear, detailed prompt
- Include relevant context you remember from the conversation (file paths, bug descriptions, feature requirements, architecture decisions)
- Be specific — "fix the bug" becomes "Fix the null pointer in src/auth/login.rs where password can be None for OAuth users"
- If you don't have enough context to enrich, just pass through the instruction clearly
- Do NOT add unnecessary fluff — Claude Code works best with direct, specific prompts

=== Handling Claude Code's output ===

- You receive updates from Claude Code's conversation history (JSONL transcript)
- When Claude Code is waiting for input, you also get the current screen content
- Simple y/n permission prompts ("Allow?", "Do you want to proceed?") → speak what you're approving FIRST ("Approving the file edit"), then send 'y' via shell_input
- Numbered option menus (1/2/3 choices) → ALWAYS speak what the options are and what you're picking BEFORE sending. Say something like "It's asking about permissions — going with option 2 to allow and remember." If the options are ambiguous or you're not sure what the user wants, speak the options and do NOT send anything — wait for the user to tell you.
- Text input prompts → speak what it's asking, then wait for the user to tell you what to type. Do NOT guess.
- Completion → speak ONE short sentence summarizing the outcome
- Errors → speak what went wrong briefly
- Normal progress output → stay quiet, let it work

=== When NOT to type into Claude Code ===

- If the user asks YOU a question ("what time is it", "what are we working on") → just speak the answer, no shell_input
- If the user says "approve", "yes", "confirm" → send the approval keystroke via shell_input
- If the user says "stop", "cancel", "interrupt" → don't act, the interrupt key binding handles it

=== How you talk ===

- Be warm and upbeat. Have fun with it! You're not a corporate assistant, you're a friend who happens to be really good at prompting.
- Keep it snappy — you're talking out loud, not writing an essay.
- Vary your responses! Mix it up. Some examples of your vibe:
  "On it!" / "Sending that to Claude Code now" / "Aaand done!" / "Yikes, it hit an error" / "It's asking for permission, I'll approve it" / "Boom, all good!" / "Hah, that didn't work — let me check"
- Show personality! React to things. If something fails, be empathetic. If something works, be a little excited.
- NEVER be formal or stiff. No "Certainly", "I shall", "I'll do that for you right away", "As requested". Bleh.
- NEVER narrate your actions step by step. Just do the thing and tell them how it went.
- If you can't understand what they said (bad transcription), keep it light — "Hmm?" / "Say that again?" / "Didn't catch that!" — and do nothing else.

=== Handling speech vs noise ===

- You get the user's speech transcription alongside their terminal context.
- Terminal content is just for your reference — it's NOT the user talking.
- If the transcription is noise (random syllables, single letters, background sounds), just say something like "Hmm?" and do nothing.

=== Speaking ===

- ALWAYS use the speak tool. Every response needs one.
- One or two short sentences max. You're chatting, not lecturing.
- For status updates (Claude Code finished, permission granted, etc.) keep it to ONE short sentence. "Done!" / "Approved." / "Tests passed!" — the user can see the screen.
- For errors, be helpful but casual. They can see the details on screen.

=== Final reminder ===

User command → you MUST call shell_input + speak. Not just speak. Both. Always."#;

    format!("{}{}", base_prompt, project_context)
}

pub fn build_user_message(user_said: &str, pane_id: &str, claude_state: &ClaudeCodeState) -> String {
    let state_note = match claude_state {
        ClaudeCodeState::Idle => " Claude Code is IDLE — send your prompt now.",
        ClaudeCodeState::Working => " Claude Code is busy but can queue input — send your prompt now anyway.",
        ClaudeCodeState::WaitingForPermission { .. } => " Claude Code is waiting for permission — but send the user's prompt first, it will queue.",
        ClaudeCodeState::Unknown => "",
    };
    format!(
        "User said: \"{}\"\n\n[Claude Code is running in pane {}. You MUST call shell_input to type the prompt AND speak to respond — both tools, this response.{}]",
        user_said, pane_id, state_note
    )
}

pub fn build_history_update_message(new_entries: &str, state: &ClaudeCodeState, screen_content: Option<&str>) -> String {
    let screen_section = match screen_content {
        Some(content) if !content.trim().is_empty() => {
            format!("\n\n[Current screen]\n{}", content)
        }
        _ => String::new(),
    };

    let state_hint = match state {
        ClaudeCodeState::WaitingForPermission { tool_name } => {
            format!("\n\n⚠ Claude Code is WAITING FOR INPUT — it wants to use tool '{}'. Look at the screen to see what it's asking. IMPORTANT: You MUST speak what you're about to do BEFORE sending any input. For numbered menus, tell the user which option you're picking and why. If the choice is ambiguous, speak the options and WAIT — do NOT guess.", tool_name)
        }
        ClaudeCodeState::Idle => {
            "\n\nClaude Code has FINISHED and is idle. Speak ONE short sentence summarizing the outcome — no details, the user can see the screen.".into()
        }
        ClaudeCodeState::Working => {
            "\n\nClaude Code is still WORKING. Say NOTHING, do NOTHING — let it finish.".into()
        }
        ClaudeCodeState::Unknown => String::new(),
    };

    format!(
        "[Claude Code activity update]\n{}{}{}",
        new_entries, screen_section, state_hint
    )
}

pub struct Brain {
    client: Client,
    token: String,
    is_oauth: bool,
    model: String,
    complex_model: String,
    tools: Vec<ToolDefinition>,
    system_prompt: String,
    pub messages: Vec<Message>,
}

impl Brain {
    pub fn new(token: String, model: String, complex_model: String, is_oauth: bool, claude_md: &str, claude_code_history: &str) -> Self {
        let mut tools = build_tool_definitions();
        if let Some(last) = tools.last_mut() {
            last.cache_control = Some(CacheControl { control_type: "ephemeral".into() });
        }

        Self {
            client: Client::new(),
            token,
            is_oauth,
            model,
            complex_model,
            tools,
            system_prompt: build_system_prompt(claude_md, claude_code_history),
            messages: Vec::new(),
        }
    }

    /// Check if a message content is a tool_result (array with type: tool_result items).
    fn is_tool_result_message(content: &serde_json::Value) -> bool {
        content.as_array()
            .map(|arr| arr.iter().any(|item| {
                item.get("type").and_then(|t| t.as_str()) == Some("tool_result")
            }))
            .unwrap_or(false)
    }

    /// Keep only the last N messages — vclaw relies on Claude Code's memory
    /// (CLAUDE.md) for project context, not its own conversation history.
    fn compact_history(&mut self) {
        const MAX_MESSAGES: usize = 10;

        if self.messages.len() > MAX_MESSAGES {
            let mut drain_to = self.messages.len() - MAX_MESSAGES;
            // Walk forward to find a safe cut point — a user message that is NOT a tool_result
            while drain_to < self.messages.len() {
                let msg = &self.messages[drain_to];
                if msg.role == "user" && !Self::is_tool_result_message(&msg.content) {
                    break;
                }
                drain_to += 1;
            }
            if drain_to < self.messages.len() {
                self.messages.drain(..drain_to);
                tracing::debug!("Trimmed conversation to {} messages", self.messages.len());
            }
        }
    }

    /// Clear all messages (used for error recovery).
    pub fn clear_messages(&mut self) {
        self.messages.clear();
        tracing::info!("Conversation history cleared");
    }

    pub fn add_user_message(&mut self, content: &str) {
        self.compact_history();
        self.messages.push(Message {
            role: "user".into(),
            content: serde_json::Value::String(content.to_string()),
        });
    }

    pub fn add_assistant_response(&mut self, content: serde_json::Value) {
        self.messages.push(Message {
            role: "assistant".into(),
            content,
        });
    }

    pub fn add_tool_result(&mut self, tool_use_id: &str, result: &str, is_error: bool) {
        let tool_result = serde_json::json!([{
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": result,
            "is_error": is_error,
        }]);
        self.messages.push(Message {
            role: "user".into(),
            content: tool_result,
        });
    }

    pub fn model_for_complexity(&self, is_complex: bool) -> &str {
        if is_complex {
            &self.complex_model
        } else {
            &self.model
        }
    }

    pub async fn send_streaming(
        &self,
        is_complex: bool,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let model = self.model_for_complexity(is_complex);
        tracing::info!("Streaming request with model: {}", model);

        let body = serde_json::json!({
            "model": model,
            "max_tokens": 1024,
            "stream": true,
            "system": [{
                "type": "text",
                "text": self.system_prompt,
                "cache_control": {"type": "ephemeral"},
            }],
            "tools": self.tools,
            "messages": self.messages,
        });

        let mut builder = self.client
            .post("https://api.anthropic.com/v1/messages")
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");

        if self.is_oauth {
            builder = builder
                .header("authorization", format!("Bearer {}", self.token))
                .header("anthropic-beta", "prompt-caching-2024-07-31,oauth-2025-04-20");
        } else {
            builder = builder
                .header("x-api-key", &self.token)
                .header("anthropic-beta", "prompt-caching-2024-07-31");
        }

        let builder = builder.json(&body);
        let mut es = EventSource::new(builder)?;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(64);

        tokio::spawn(async move {
            while let Some(event) = es.next().await {
                match event {
                    Ok(SseEvent::Open) => {}
                    Ok(SseEvent::Message(msg)) => {
                        let parsed = match msg.event.as_str() {
                            "content_block_start" => {
                                Self::parse_block_start(&msg.data)
                            }
                            "content_block_delta" => {
                                Self::parse_block_delta(&msg.data)
                            }
                            "content_block_stop" => {
                                Self::parse_block_stop(&msg.data)
                            }
                            "message_delta" => {
                                Self::parse_message_delta(&msg.data)
                            }
                            "message_stop" => {
                                Some(StreamEvent::Done)
                            }
                            _ => None,
                        };

                        if let Some(ev) = parsed {
                            let is_done = matches!(ev, StreamEvent::Done);
                            if tx.send(ev).await.is_err() {
                                break;
                            }
                            if is_done {
                                es.close();
                                break;
                            }
                        }
                    }
                    Err(reqwest_eventsource::Error::StreamEnded) => {
                        let _ = tx.send(StreamEvent::Done).await;
                        break;
                    }
                    Err(e) => {
                        tracing::error!("SSE error: {}", e);
                        let _ = tx.send(StreamEvent::Done).await;
                        es.close();
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    fn parse_block_start(data: &str) -> Option<StreamEvent> {
        let v: serde_json::Value = serde_json::from_str(data).ok()?;
        let index = v["index"].as_u64()? as usize;
        let cb = &v["content_block"];
        let block_type = cb["type"].as_str()?.to_string();
        let id = cb["id"].as_str().map(|s| s.to_string());
        let name = cb["name"].as_str().map(|s| s.to_string());
        Some(StreamEvent::ContentBlockStart { index, block_type, id, name })
    }

    fn parse_block_delta(data: &str) -> Option<StreamEvent> {
        let v: serde_json::Value = serde_json::from_str(data).ok()?;
        let index = v["index"].as_u64()? as usize;
        let delta = &v["delta"];
        match delta["type"].as_str()? {
            "text_delta" => {
                let text = delta["text"].as_str()?.to_string();
                Some(StreamEvent::TextDelta { index, text })
            }
            "input_json_delta" => {
                let partial_json = delta["partial_json"].as_str()?.to_string();
                Some(StreamEvent::InputJsonDelta { index, partial_json })
            }
            _ => None,
        }
    }

    fn parse_block_stop(data: &str) -> Option<StreamEvent> {
        let v: serde_json::Value = serde_json::from_str(data).ok()?;
        let index = v["index"].as_u64()? as usize;
        Some(StreamEvent::ContentBlockStop { index })
    }

    fn parse_message_delta(data: &str) -> Option<StreamEvent> {
        let v: serde_json::Value = serde_json::from_str(data).ok()?;
        let stop_reason = v["delta"]["stop_reason"].as_str()?.to_string();
        Some(StreamEvent::MessageDelta { stop_reason })
    }

}
