use anyhow::{Context, Result};
use std::io::{self, Write};
use std::time::Duration;
use termimad::{MadSkin, terminal_size};
use time::OffsetDateTime;
use tracing::{debug, info};

use crate::agent::AgentRuntime;
use crate::config::SkinMode;
use crate::session_capture::SessionCapture;
use crate::terminal_session::{TerminalSession, TerminalSnapshot};

pub struct ReplOptions<'a> {
    pub wait_ms: u64,
    pub initial_prompt: &'a str,
    pub skin_mode: SkinMode,
    pub verbose: u8,
    pub api_key: &'a str,
    pub yolo: bool,
}

pub async fn run_repl(
    session: &TerminalSession,
    options: ReplOptions<'_>,
    session_capture: Option<SessionCapture>,
) -> Result<()> {
    let mut agent_runtime = AgentRuntime::new(
        session.handle(),
        options.initial_prompt,
        options.api_key,
        options.yolo,
        session_capture.clone(),
    )?;
    let default_wait_seconds = Duration::from_millis(options.wait_ms).as_secs_f64();
    let skin = resolve_skin(options.skin_mode);
    let mut last_response_total_tokens: Option<u64> = None;

    info!("interactive mode: prompts go to agent; commands: :raw, :snap, :reset, :help, :quit");

    loop {
        print_repl_prompt(&skin, last_response_total_tokens)?;

        let Some(line) = read_repl_line().await? else {
            break;
        };

        if let LineControl::Quit = process_line(
            session,
            &mut agent_runtime,
            &options,
            session_capture.as_ref(),
            &skin,
            default_wait_seconds,
            &line,
            &mut last_response_total_tokens,
        )
        .await?
        {
            break;
        }
    }

    Ok(())
}

pub async fn run_single_command(
    session: &TerminalSession,
    options: ReplOptions<'_>,
    line: &str,
    session_capture: Option<SessionCapture>,
) -> Result<()> {
    let mut agent_runtime = AgentRuntime::new(
        session.handle(),
        options.initial_prompt,
        options.api_key,
        options.yolo,
        session_capture.clone(),
    )?;
    let default_wait_seconds = Duration::from_millis(options.wait_ms).as_secs_f64();
    let skin = resolve_skin(options.skin_mode);
    let mut last_response_total_tokens = None;
    process_line(
        session,
        &mut agent_runtime,
        &options,
        session_capture.as_ref(),
        &skin,
        default_wait_seconds,
        line,
        &mut last_response_total_tokens,
    )
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineControl {
    Continue,
    Quit,
}

async fn process_line(
    session: &TerminalSession,
    agent_runtime: &mut AgentRuntime,
    options: &ReplOptions<'_>,
    session_capture: Option<&SessionCapture>,
    skin: &MadSkin,
    default_wait_seconds: f64,
    line: &str,
    last_response_total_tokens: &mut Option<u64>,
) -> Result<LineControl> {
    let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
    if !trimmed.is_empty()
        && let Some(capture) = session_capture
    {
        capture.record_user_input(trimmed);
    }

    match trimmed {
        "" => return Ok(LineControl::Continue),
        ":quit" | ":q" => return Ok(LineControl::Quit),
        ":help" => {
            eprintln!(
                "commands: :raw <spec> (send escaped bytes), :snap (snapshot now), :reset (restart shell + clear agent state), :quit (exit). every other line is sent to the agent"
            );
            return Ok(LineControl::Continue);
        }
        ":snap" => {
            let snapshot = session.snapshot().await?;
            print_snapshot(&snapshot, options.verbose);
            return Ok(LineControl::Continue);
        }
        ":reset" => {
            session.reset().await?;
            agent_runtime.reset();
            *last_response_total_tokens = None;
            let snapshot = session.snapshot().await?;
            print_snapshot(&snapshot, options.verbose);
            return Ok(LineControl::Continue);
        }
        _ => {}
    }

    if trimmed.starts_with(':') {
        match parse_prefixed_command(trimmed) {
            Some(command) => {
                match execute_prefixed_command(agent_runtime, command, default_wait_seconds).await {
                    Ok(snapshot) => println!("{snapshot}"),
                    Err(err) => eprintln!("command error: {err}"),
                }
            }
            None => eprintln!("command error: unknown command `{trimmed}`"),
        }
        return Ok(LineControl::Continue);
    }

    match agent_runtime.prompt(trimmed).await {
        Ok(response) => {
            *last_response_total_tokens = Some(response.total_tokens);
            print_agent_response(skin, &response.output);
            if let Some(capture) = session_capture {
                capture.record_assistant_response(&response.output);
            }
        }
        Err(err) => eprintln!("agent error: {err}"),
    }

    Ok(LineControl::Continue)
}

#[derive(Debug, PartialEq, Eq)]
enum PrefixedCommand {
    Raw(String),
}

fn parse_prefixed_command(line: &str) -> Option<PrefixedCommand> {
    parse_prefixed_arg(line, ":raw").map(PrefixedCommand::Raw)
}

fn parse_prefixed_arg(line: &str, prefix: &str) -> Option<String> {
    if line == prefix {
        return Some(String::new());
    }

    let rest = line.strip_prefix(prefix)?;
    if !rest.chars().next().is_some_and(|ch| ch.is_whitespace()) {
        return None;
    }

    Some(rest.trim_start().to_string())
}

async fn execute_prefixed_command(
    agent_runtime: &AgentRuntime,
    command: PrefixedCommand,
    wait_seconds: f64,
) -> Result<String> {
    match command {
        PrefixedCommand::Raw(spec) => agent_runtime.send_raw_input(&spec, wait_seconds).await,
    }
}

pub fn print_snapshot(snapshot: &TerminalSnapshot, verbose: u8) {
    if verbose > 1 {
        debug!(
            "snapshot: {}x{}, cursor={:?}, lines={}",
            snapshot.cols,
            snapshot.rows,
            snapshot.cursor,
            snapshot.lines.len()
        );
    } else if verbose > 0 {
        info!(
            "snapshot: {}x{}, cursor={:?}, lines={}",
            snapshot.cols,
            snapshot.rows,
            snapshot.cursor,
            snapshot.lines.len()
        );
    }

    println!("{}", snapshot.render());
}

fn print_repl_prompt(skin: &MadSkin, last_response_total_tokens: Option<u64>) -> Result<()> {
    let (width, _) = terminal_size();
    let separator = "─".repeat(usize::from(width.max(1)));
    let timestamp = current_timestamp_hms();
    let token_count = last_response_total_tokens
        .map(|tokens| tokens.to_string())
        .unwrap_or_else(|| "n/a".to_string());
    let prompt = format!("*{timestamp}* **{token_count}** ❯ ");

    println!("{}", skin.inline(&separator));
    print!("{}", skin.inline(&prompt));
    io::stdout().flush().context("failed to flush repl prompt")
}

fn print_agent_response(skin: &MadSkin, response: &str) {
    skin.print_text(response);
}

fn resolve_skin(skin_mode: SkinMode) -> MadSkin {
    match skin_mode {
        SkinMode::Light => MadSkin::default_light(),
        SkinMode::Dark => MadSkin::default_dark(),
        SkinMode::Default => MadSkin::default(),
    }
}

fn current_timestamp_hms() -> String {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let (hours, minutes, seconds) = now.time().as_hms();
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

#[cfg(test)]
mod skin_tests {
    use super::resolve_skin;
    use crate::config::SkinMode;

    #[test]
    fn resolves_each_skin_mode() {
        let _ = resolve_skin(SkinMode::Default);
        let _ = resolve_skin(SkinMode::Light);
        let _ = resolve_skin(SkinMode::Dark);
    }
}

async fn read_repl_line() -> Result<Option<String>> {
    tokio::task::spawn_blocking(|| -> io::Result<Option<String>> {
        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(line)),
            Err(err) => Err(err),
        }
    })
    .await
    .context("failed to join repl input reader")?
    .context("failed to read repl line")
}

#[cfg(test)]
mod tests {
    use super::{PrefixedCommand, current_timestamp_hms, parse_prefixed_command};

    #[test]
    fn parses_raw_with_tab_separated_payload() {
        let parsed = parse_prefixed_command(":raw\t\\x03");
        assert_eq!(parsed, Some(PrefixedCommand::Raw("\\x03".to_string())));
    }

    #[test]
    fn rejects_non_separated_prefix() {
        assert_eq!(parse_prefixed_command(":run echo hi"), None);
        assert_eq!(parse_prefixed_command(":raw\\x03"), None);
    }

    #[test]
    fn keeps_empty_payload_for_usage_errors() {
        assert_eq!(
            parse_prefixed_command(":raw"),
            Some(PrefixedCommand::Raw(String::new()))
        );
    }

    #[test]
    fn current_timestamp_has_hms_shape() {
        let ts = current_timestamp_hms();
        assert_eq!(ts.len(), 8);
        assert_eq!(ts.as_bytes()[2], b':');
        assert_eq!(ts.as_bytes()[5], b':');
    }
}
