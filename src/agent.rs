use anyhow::{Context, Result, ensure};
use rig::agent::Agent;
use rig::client::CompletionClient;
use rig::completion::{Message, Prompt, ToolDefinition};
use rig::providers::openai;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::session_capture::SessionCapture;
use crate::terminal_session::TerminalSessionHandle;

const DEFAULT_MAX_TURNS: usize = 1_000_000;
const AGENT_MODEL: &str = "gpt-5.2";

type OpenAiAgent = Agent<<openai::Client as CompletionClient>::CompletionModel>;

pub struct AgentRuntime {
    agent: OpenAiAgent,
    chat_history: Vec<Message>,
    tool_context: Arc<ShellToolContext>,
}

pub struct AgentPromptResponse {
    pub output: String,
    pub total_tokens: u64,
}

impl AgentRuntime {
    pub fn new(
        session: TerminalSessionHandle,
        initial_prompt: &str,
        api_key: &str,
        yolo: bool,
        session_capture: Option<SessionCapture>,
    ) -> Result<Self> {
        let client: openai::Client =
            openai::Client::new(api_key).context("failed to create OpenAI client")?;
        let tool_context = Arc::new(ShellToolContext::new(session, yolo, session_capture));

        let agent = client
            .agent(AGENT_MODEL)
            .preamble(initial_prompt)
            .default_max_turns(DEFAULT_MAX_TURNS)
            .tool(RawInputTool::new(tool_context.clone()))
            .build();

        Ok(Self {
            agent,
            chat_history: Vec::new(),
            tool_context,
        })
    }

    pub async fn prompt(&mut self, input: &str) -> Result<AgentPromptResponse> {
        let response = self
            .agent
            .prompt(input)
            .with_history(&mut self.chat_history)
            .with_tool_concurrency(1)
            .extended_details()
            .await
            .map_err(anyhow::Error::from)?;

        Ok(AgentPromptResponse {
            output: response.output,
            total_tokens: response.total_usage.total_tokens,
        })
    }

    pub async fn send_raw_input(&self, spec: &str, wait_seconds: f64) -> Result<String> {
        // FIXME: handle errors gracefully
        ensure!(!spec.is_empty(), "usage: :raw <escaped bytes>");
        let bytes = decode_terminal_input(spec)?;
        self.tool_context
            .execute_user_input(bytes, wait_seconds)
            .await
    }

    pub fn reset(&mut self) {
        self.chat_history.clear();
    }
}

fn decode_terminal_input(spec: &str) -> Result<Vec<u8>> {
    let chars: Vec<char> = spec.chars().collect();
    let mut out = Vec::with_capacity(spec.len());
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' {
            i += 1;
            if i >= chars.len() {
                anyhow::bail!("dangling trailing backslash in input: {spec:?}");
            }

            match chars[i] {
                'n' => out.push(b'\n'),
                'r' => out.push(b'\r'),
                't' => out.push(b'\t'),
                '\\' => out.push(b'\\'),
                'x' => {
                    if i + 2 >= chars.len() {
                        anyhow::bail!("expected two hex digits after \\x in input: {spec:?}");
                    }
                    let hi = chars[i + 1]
                        .to_digit(16)
                        .context("invalid first hex digit in \\xNN escape")?;
                    let lo = chars[i + 2]
                        .to_digit(16)
                        .context("invalid second hex digit in \\xNN escape")?;
                    out.push(((hi << 4) | lo) as u8);
                    i += 2;
                }
                ch => anyhow::bail!("unsupported escape sequence: \\{ch}"),
            }

            i += 1;
            continue;
        }

        let mut tmp = [0_u8; 4];
        out.extend_from_slice(chars[i].encode_utf8(&mut tmp).as_bytes());
        i += 1;
    }

    Ok(out)
}

#[derive(Clone)]
struct ShellToolContext {
    session: TerminalSessionHandle,
    yolo: bool,
    session_capture: Option<SessionCapture>,
    execution_lock: Arc<Mutex<()>>,
}

impl ShellToolContext {
    fn new(
        session: TerminalSessionHandle,
        yolo: bool,
        session_capture: Option<SessionCapture>,
    ) -> Self {
        Self {
            session,
            yolo,
            session_capture,
            execution_lock: Arc::new(Mutex::new(())),
        }
    }

    fn record_tool_call<T: Serialize>(&self, tool_name: &str, params: &T, snapshot: &str) {
        if let Some(session_capture) = self.session_capture.as_ref() {
            session_capture.record_tool_call(tool_name, params, snapshot);
        }
    }

    async fn maybe_confirm(&self, tool_name: &str, spec: &str, bytes: &[u8]) -> Result<bool> {
        if self.yolo {
            return Ok(true);
        }

        let tool_name = tool_name.to_string();
        let spec = spec.to_string();
        let preview = render_bytes(bytes);

        tokio::task::spawn_blocking(move || -> Result<bool> {
            eprintln!();
            eprintln!("approval required for LLM tool call");
            eprintln!("tool: {tool_name}");
            eprintln!("input: {spec}");
            eprintln!("bytes: {preview}");
            print!("allow sending these bytes to the shell? [y/N]: ");
            io::stdout()
                .flush()
                .context("failed to flush confirmation prompt")?;

            let mut answer = String::new();
            io::stdin()
                .read_line(&mut answer)
                .context("failed to read confirmation response")?;

            let answer = answer.trim().to_ascii_lowercase();
            Ok(matches!(answer.as_str(), "y" | "yes"))
        })
        .await
        .context("failed to join confirmation prompt task")?
    }

    async fn execute_tool_call(
        &self,
        tool_name: &str,
        spec: &str,
        bytes: Vec<u8>,
        wait_seconds: f64,
    ) -> Result<String> {
        validate_wait_seconds(wait_seconds)?;

        let _lock = self.execution_lock.lock().await;
        if !self.maybe_confirm(tool_name, spec, &bytes).await? {
            let snapshot = self.session.snapshot().await?;
            return Ok(format!(
                "User denied the `{tool_name}` tool call. No bytes were sent.\n\n{}",
                snapshot.render()
            ));
        }

        self.execute_locked(bytes, wait_seconds).await
    }

    async fn execute_user_input(&self, bytes: Vec<u8>, wait_seconds: f64) -> Result<String> {
        validate_wait_seconds(wait_seconds)?;

        let _lock = self.execution_lock.lock().await;
        self.execute_locked(bytes, wait_seconds).await
    }

    async fn execute_locked(&self, bytes: Vec<u8>, wait_seconds: f64) -> Result<String> {
        debug_assert!(wait_seconds >= 0.0 && wait_seconds.is_finite());

        self.session.send_input(bytes).await?;

        if wait_seconds > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(wait_seconds)).await;
        }

        Ok(self.session.snapshot().await?.render())
    }
}

#[derive(Deserialize, Serialize)]
struct ShellInputArgs {
    str: String,
    float: f64,
}

#[derive(Debug)]
struct ShellToolError {
    message: String,
}

impl ShellToolError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ShellToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ShellToolError {}

impl From<anyhow::Error> for ShellToolError {
    fn from(err: anyhow::Error) -> Self {
        Self::new(err.to_string())
    }
}

#[derive(Clone)]
struct RawInputTool {
    context: Arc<ShellToolContext>,
}

impl RawInputTool {
    fn new(context: Arc<ShellToolContext>) -> Self {
        Self { context }
    }
}

impl Tool for RawInputTool {
    const NAME: &'static str = "raw_input";
    type Error = ShellToolError;
    type Args = ShellInputArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Decode the escaped input string and send the exact bytes to the terminal. Returns a snapshot after waiting float seconds.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "str": {
                        "type": "string",
                        "description": "Escaped bytes spec (supports \\n, \\r, \\t, \\xNN, \\\\)"
                    },
                    "float": {
                        "type": "number",
                        "description": "Seconds to wait before capturing the terminal snapshot"
                    }
                },
                "required": ["str", "float"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // FIXME: handle errors gracefully
        validate_wait_seconds(args.float)?;

        let bytes = decode_terminal_input(&args.str)?;
        let spec = format!("{:#?}", args.str);

        let snapshot = self
            .context
            .execute_tool_call(Self::NAME, &spec, bytes, args.float)
            .await?;

        self.context.record_tool_call(Self::NAME, &args, &snapshot);
        Ok(snapshot)
    }
}

fn validate_wait_seconds(wait_seconds: f64) -> Result<()> {
    ensure!(wait_seconds.is_finite(), "float must be a finite number");
    ensure!(wait_seconds >= 0.0, "float must be non-negative");
    Ok(())
}

fn render_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for &byte in bytes {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(byte as char),
            _ => {
                let _ = write!(&mut out, "\\x{byte:02X}");
            }
        }
    }

    out
}
