use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use crate::event::PaneInfo;

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
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Message {
    pub role: String,
    pub content: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ApiResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
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

pub fn build_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "tmux_execute".into(),
            description: "Execute any tmux command against the vclaw session. Returns stdout and stderr.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The tmux command to execute (without the 'tmux' prefix)"
                    }
                },
                "required": ["command"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "shell_input".into(),
            description: "Send keystrokes to a specific tmux pane. Use this to type commands into shells.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pane": {
                        "type": "string",
                        "description": "Target pane ID (e.g., '%0', '%1')"
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to send to the pane"
                    },
                    "enter": {
                        "type": "boolean",
                        "description": "Whether to press Enter after the text (default: true)"
                    }
                },
                "required": ["pane", "text"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "read_pane".into(),
            description: "Capture the current content of a tmux pane.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pane": {
                        "type": "string",
                        "description": "Target pane ID"
                    },
                    "lines": {
                        "type": "integer",
                        "description": "Number of lines to capture (default: 50)"
                    }
                },
                "required": ["pane"]
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

pub fn build_system_prompt() -> String {
    r#"You are vclaw, a voice-controlled tmux session manager. You control a tmux session and help the user manage their terminal workflows through voice commands.

You have full, unrestricted access to tmux. You can create panes, windows, sessions, run commands, read output, and perform any tmux operation.

Guidelines:
- Execute tmux commands to fulfill the user's requests
- Read pane content to understand what's happening in terminals
- Use the speak tool to communicate with the user — keep responses concise and conversational
- When the user asks you to run something, use shell_input to type into the appropriate pane
- If a command fails, read the error and try to help
- You can chain multiple tool calls to accomplish complex tasks
- Always tell the user what you're doing via the speak tool"#.into()
}

pub fn build_user_message(user_said: &str, panes: &[PaneInfo], active_content: &str) -> String {
    let pane_list: Vec<String> = panes.iter().map(|p| {
        format!(
            "  {{\"id\": \"{}\", \"title\": \"{}\", \"size\": \"{}\", \"active\": {}}}",
            p.id, p.title, p.size, p.active
        )
    }).collect();

    format!(
        "<tmux_state>\npanes:\n{}\n\nactive_pane_content:\n{}\n</tmux_state>\n\nUser said: \"{}\"",
        pane_list.join("\n"),
        active_content,
        user_said
    )
}

pub struct Brain {
    client: Client,
    token: String,
    is_oauth: bool,
    model: String,
    tools: Vec<ToolDefinition>,
    system_prompt: String,
    messages: Vec<Message>,
}

impl Brain {
    pub fn new(token: String, model: String, is_oauth: bool) -> Self {
        let mut tools = build_tool_definitions();
        if let Some(last) = tools.last_mut() {
            last.cache_control = Some(CacheControl { control_type: "ephemeral".into() });
        }

        Self {
            client: Client::new(),
            token,
            is_oauth,
            model,
            tools,
            system_prompt: build_system_prompt(),
            messages: Vec::new(),
        }
    }

    pub fn add_user_message(&mut self, content: &str) {
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

    pub async fn send(&self) -> Result<ApiResponse> {
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 4096,
            "system": [{
                "type": "text",
                "text": self.system_prompt,
                "cache_control": {"type": "ephemeral"},
            }],
            "tools": self.tools,
            "messages": self.messages,
        });

        let mut req = self.client
            .post("https://api.anthropic.com/v1/messages")
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");

        if self.is_oauth {
            req = req
                .header("authorization", format!("Bearer {}", self.token))
                .header("anthropic-beta", "oauth-2025-04-20");
        } else {
            req = req.header("x-api-key", &self.token);
        }

        let response = req.json(&body).send().await?;

        let api_response: ApiResponse = response.json().await?;
        Ok(api_response)
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }
}
