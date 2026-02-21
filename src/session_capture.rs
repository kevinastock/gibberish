use anyhow::{Context, Result};
use markdown::to_html;
use serde::Serialize;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;

#[derive(Clone)]
pub struct SessionCapture {
    inner: Arc<Mutex<SessionCaptureInner>>,
}

struct SessionCaptureInner {
    started_at: String,
    events: Vec<SessionEvent>,
}

#[derive(Clone)]
enum SessionEvent {
    UserInput {
        timestamp: String,
        text: String,
    },
    ToolCall {
        timestamp: String,
        tool_name: String,
        params_json: String,
        snapshot: String,
    },
    AssistantResponse {
        timestamp: String,
        markdown: String,
    },
}

impl SessionCapture {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionCaptureInner {
                started_at: now_timestamp(),
                events: Vec::new(),
            })),
        }
    }

    pub fn record_user_input(&self, text: &str) {
        self.push_event(SessionEvent::UserInput {
            timestamp: now_timestamp(),
            text: text.to_string(),
        });
    }

    pub fn record_tool_call<T: Serialize>(&self, tool_name: &str, params: &T, snapshot: &str) {
        let params_json = match serde_json::to_string_pretty(params) {
            Ok(json) => json,
            Err(err) => format!("{{\n  \"serialization_error\": {:?}\n}}", err.to_string()),
        };

        self.push_event(SessionEvent::ToolCall {
            timestamp: now_timestamp(),
            tool_name: tool_name.to_string(),
            params_json,
            snapshot: snapshot.to_string(),
        });
    }

    pub fn record_assistant_response(&self, markdown: &str) {
        self.push_event(SessionEvent::AssistantResponse {
            timestamp: now_timestamp(),
            markdown: markdown.to_string(),
        });
    }

    pub fn write_html(&self, path: &Path) -> Result<()> {
        let html = self.render_html();
        fs::write(path, html)
            .with_context(|| format!("failed to write session HTML to {}", path.display()))
    }

    fn push_event(&self, event: SessionEvent) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.events.push(event);
        }
    }

    fn render_html(&self) -> String {
        let (started_at, events) = {
            let inner = self
                .inner
                .lock()
                .expect("session capture mutex should not be poisoned");
            (inner.started_at.clone(), inner.events.clone())
        };

        let now = now_timestamp();
        let mut user_inputs = 0_usize;
        let mut tool_calls = 0_usize;
        let mut assistant_responses = 0_usize;

        for event in &events {
            match event {
                SessionEvent::UserInput { .. } => user_inputs += 1,
                SessionEvent::ToolCall { .. } => tool_calls += 1,
                SessionEvent::AssistantResponse { .. } => assistant_responses += 1,
            }
        }

        let mut out = String::new();
        out.push_str("<!doctype html>\n");
        out.push_str("<html lang=\"en\">\n");
        out.push_str("<head>\n");
        out.push_str("  <meta charset=\"utf-8\">\n");
        out.push_str(
            "  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n",
        );
        out.push_str("  <title>gibberish session capture</title>\n");
        out.push_str("  <link rel=\"icon\" href=\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><text y='.9em' font-size='90'>🪵</text></svg>\" />\n");
        out.push_str("  <style>\n");
        out.push_str("    :root { color-scheme: light dark; }\n");
        out.push_str(
            "    body { margin: 0; font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif; background: #f7f7f8; color: #1f2328; }\n",
        );
        out.push_str(
            "    .container { max-width: 1100px; margin: 0 auto; padding: 1.5rem 1rem 2rem; }\n",
        );
        out.push_str(
            "    .summary { background: #ffffff; border: 1px solid #d0d7de; border-radius: 10px; padding: 1rem; margin-bottom: 1rem; }\n",
        );
        out.push_str("    .summary h1 { margin: 0 0 0.6rem; font-size: 1.2rem; }\n");
        out.push_str("    .summary p { margin: 0.2rem 0; }\n");
        out.push_str(
            "    .event { background: #ffffff; border: 1px solid #d0d7de; border-left-width: 6px; border-radius: 10px; margin: 0 0 1rem; padding: 0.8rem 1rem 1rem; }\n",
        );
        out.push_str("    .event.user { border-left-color: #0a7c3e; }\n");
        out.push_str("    .event.tool { border-left-color: #0969da; }\n");
        out.push_str("    .event.assistant { border-left-color: #8250df; }\n");
        out.push_str("    .event h2 { margin: 0; font-size: 1rem; }\n");
        out.push_str("    .meta { margin-top: 0.2rem; color: #59636e; font-size: 0.85rem; }\n");
        out.push_str("    .label { font-weight: 600; margin: 0.8rem 0 0.3rem; display: block; }\n");
        out.push_str(
            "    pre { margin: 0; border: 1px solid #d0d7de; border-radius: 8px; padding: 0.75rem; overflow-x: auto; white-space: pre-wrap; background: #f6f8fa; font-family: SFMono-Regular, Menlo, Consolas, monospace; font-size: 0.85rem; line-height: 1.45; }\n",
        );
        out.push_str(
            "    .assistant-body { border: 1px solid #d0d7de; border-radius: 8px; padding: 0.75rem 0.9rem; background: #ffffff; }\n",
        );
        out.push_str("    .assistant-body > :first-child { margin-top: 0; }\n");
        out.push_str("    .assistant-body > :last-child { margin-bottom: 0; }\n");
        out.push_str("  </style>\n");
        out.push_str("</head>\n");
        out.push_str("<body>\n");
        out.push_str("  <main class=\"container\">\n");
        out.push_str("    <section class=\"summary\">\n");
        out.push_str("      <h1>gibberish session capture</h1>\n");
        let _ = writeln!(
            &mut out,
            "      <p><strong>Started:</strong> {}</p>",
            escape_html(&started_at)
        );
        let _ = writeln!(
            &mut out,
            "      <p><strong>Generated:</strong> {}</p>",
            escape_html(&now)
        );
        let _ = writeln!(
            &mut out,
            "      <p><strong>User inputs:</strong> {} | <strong>Tool calls:</strong> {} | <strong>Assistant responses:</strong> {}</p>",
            user_inputs, tool_calls, assistant_responses
        );
        out.push_str("    </section>\n");

        for (idx, event) in events.iter().enumerate() {
            match event {
                SessionEvent::UserInput { timestamp, text } => {
                    out.push_str("    <section class=\"event user\">\n");
                    let _ = writeln!(&mut out, "      <h2>#{} User Input</h2>", idx + 1);
                    let _ = writeln!(
                        &mut out,
                        "      <div class=\"meta\">{}</div>",
                        escape_html(timestamp)
                    );
                    out.push_str("      <span class=\"label\">Command</span>\n");
                    let _ = writeln!(&mut out, "      <pre>{}</pre>", escape_html(text));
                    out.push_str("    </section>\n");
                }
                SessionEvent::ToolCall {
                    timestamp,
                    tool_name,
                    params_json,
                    snapshot,
                } => {
                    out.push_str("    <section class=\"event tool\">\n");
                    let _ = writeln!(
                        &mut out,
                        "      <h2>#{} Tool Call: {}</h2>",
                        idx + 1,
                        escape_html(tool_name)
                    );
                    let _ = writeln!(
                        &mut out,
                        "      <div class=\"meta\">{}</div>",
                        escape_html(timestamp)
                    );
                    out.push_str("      <span class=\"label\">Parameters</span>\n");
                    let _ = writeln!(&mut out, "      <pre>{}</pre>", escape_html(params_json));
                    out.push_str("      <span class=\"label\">Tool Response Snapshot</span>\n");
                    let _ = writeln!(&mut out, "      <pre>{}</pre>", escape_html(snapshot));
                    out.push_str("    </section>\n");
                }
                SessionEvent::AssistantResponse {
                    timestamp,
                    markdown,
                } => {
                    out.push_str("    <section class=\"event assistant\">\n");
                    let _ = writeln!(&mut out, "      <h2>#{} Assistant Response</h2>", idx + 1);
                    let _ = writeln!(
                        &mut out,
                        "      <div class=\"meta\">{}</div>",
                        escape_html(timestamp)
                    );
                    out.push_str("      <span class=\"label\">Rendered Markdown</span>\n");
                    let _ = writeln!(
                        &mut out,
                        "      <div class=\"assistant-body\">{}</div>",
                        markdown_to_html(markdown)
                    );
                    out.push_str("    </section>\n");
                }
            }
        }

        out.push_str("  </main>\n");
        out.push_str("</body>\n");
        out.push_str("</html>\n");
        out
    }
}

fn markdown_to_html(markdown: &str) -> String {
    to_html(markdown)
}

fn now_timestamp() -> String {
    OffsetDateTime::now_local()
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .to_string()
}

fn escape_html(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        push_escaped_char(&mut escaped, ch);
    }
    escaped
}

fn push_escaped_char(out: &mut String, ch: char) {
    match ch {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '\'' => out.push_str("&#39;"),
        '"' => out.push_str("&quot;"),
        _ => out.push(ch),
    }
}

#[cfg(test)]
mod tests {
    use super::SessionCapture;
    use serde_json::json;

    #[test]
    fn renders_full_capture_html() {
        let capture = SessionCapture::new();
        capture.record_user_input(":raw ls\\n");
        capture.record_tool_call(
            "raw_input",
            &json!({"str": "ls", "float": 0.4}),
            "output line\nCursor info: row=0, col=0, char=\"o\"",
        );
        capture.record_assistant_response("**Done**\n\n`ls` returned output.");

        let html = capture.render_html();
        assert!(html.contains("User Input"));
        assert!(html.contains("Tool Call: raw_input"));
        assert!(html.contains("Tool Response Snapshot"));
        assert!(html.contains("<strong>Done</strong>"));
        assert!(html.contains("<code>ls</code>"));
    }

    #[test]
    fn escapes_user_and_snapshot_content() {
        let capture = SessionCapture::new();
        capture.record_user_input("echo <unsafe>");
        capture.record_tool_call("raw_input", &json!({"str": "<x>", "float": 0.1}), "<snap>");

        let html = capture.render_html();
        assert!(html.contains("echo &lt;unsafe&gt;"));
        assert!(html.contains("&lt;snap&gt;"));
    }

    #[test]
    fn renders_headings_lists_and_links() {
        let capture = SessionCapture::new();
        capture.record_assistant_response(
            "# Title\n\n- one\n- two\n\nUse [docs](https://example.com).",
        );

        let html = capture.render_html();
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<ul>"));
        assert!(html.contains("<li>one</li>"));
        assert!(html.contains("<a href=\"https://example.com\">docs</a>"));
    }
}
