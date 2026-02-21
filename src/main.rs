mod agent;
mod config;
mod repl;
mod session_capture;
mod terminal_session;

use anyhow::{Context, Result};
use clap::builder::PathBufValueParser;
use clap::{ArgAction, Parser};
use repl::ReplOptions;
use session_capture::SessionCapture;
use std::io;
use std::path::PathBuf;
use terminal_session::TerminalSession;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "gibberish", version, about = "Gibberish CLI")]
struct Cli {
    /// Increase log verbosity (use -vv for more detail).
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,

    /// Path to TOML config file (default: ~/.config/gibberish/config.toml).
    #[arg(long, value_parser = PathBufValueParser::new())]
    config: Option<PathBuf>,

    /// Disable confirmation prompts for LLM-issued terminal input.
    #[arg(long)]
    yolo: bool,

    /// Execute one REPL line and exit.
    #[arg(short = 'c', value_name = "COMMAND")]
    command: Option<String>,

    /// Start the configured shell as a login shell.
    /// Accepted for compatibility; currently a no-op.
    #[arg(short = 'l', long = "login")]
    login: bool,

    /// Force the configured shell into interactive mode.
    /// Accepted for compatibility; currently a no-op.
    #[arg(short = 'i')]
    interactive: bool,

    /// Write a single-file HTML capture of the session history to this path.
    #[arg(long, value_parser = PathBufValueParser::new(), value_name = "PATH")]
    session_html: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose)?;
    let options = config::resolve_session_options(cli.config.as_deref())?;
    let wait_ms = options.wait_ms;
    let yolo = cli.yolo || options.yolo;
    let api_key = options.llm.api_key.clone();
    let initial_prompt = options.llm.initial_prompt.clone();
    let skin_mode = options.llm.skin;
    let mut session = TerminalSession::start(options).await?;
    let session_capture = cli.session_html.as_ref().map(|_| SessionCapture::new());

    let repl_result = if let Some(command) = cli.command.as_deref() {
        repl::run_single_command(
            &session,
            ReplOptions {
                wait_ms,
                initial_prompt: &initial_prompt,
                skin_mode,
                verbose: cli.verbose,
                api_key: &api_key,
                yolo,
            },
            command,
            session_capture.clone(),
        )
        .await
    } else {
        repl::run_repl(
            &session,
            ReplOptions {
                wait_ms,
                initial_prompt: &initial_prompt,
                skin_mode,
                verbose: cli.verbose,
                api_key: &api_key,
                yolo,
            },
            session_capture.clone(),
        )
        .await
    };

    let shutdown_result = session
        .shutdown()
        .await
        .context("failed to shut down terminal session");

    let capture_write_result = match (&session_capture, cli.session_html.as_deref()) {
        (Some(capture), Some(path)) => capture
            .write_html(path)
            .with_context(|| format!("failed to write session capture HTML to {}", path.display())),
        _ => Ok(()),
    };

    repl_result?;
    shutdown_result?;
    capture_write_result?;

    Ok(())
}

fn init_tracing(verbose: u8) -> Result<()> {
    let default_level = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_target(false)
        .try_init()
        .map_err(|err| anyhow::anyhow!("failed to initialize tracing: {err}"))
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn parses_combined_login_and_command_flags() {
        let cli = Cli::try_parse_from(["gibberish", "-lc", "echo hi"]).expect("parse args");

        assert!(cli.login);
        assert_eq!(cli.command.as_deref(), Some("echo hi"));
    }

    #[test]
    fn accepts_interactive_compat_flag() {
        let cli = Cli::try_parse_from(["gibberish", "-i", "-c", "echo hi"]).expect("parse args");

        assert!(cli.interactive);
        assert_eq!(cli.command.as_deref(), Some("echo hi"));
    }
}
