# GibberiSH

Some people want to make agents safe and reliable.
Some people want to make them ubiquitous.
I want to make them feel the pain of the tools we use every day.

GibberiSH is an agent with exactly one tool: it feeds raw bytes fed into
a bash PTY, and it returns a snapshot of the terminal screen after an
agent-requested delay.

Because it only looks at the screen, it can drive interactive programs like
vim, lynx, ssh, tmux, etc; just like you and I would.

If you really wanted, you could set it as your default shell. You shouldn't.
It has no sandbox and can share all your secrets with the world. But you could.

## Examples

Here's a few recorded sessions, with the tool calls shown to illustrate what it can do.

- [Process control](https://gisthost.github.io/?ddfaa18a70a4d7314fc8ada8fd2a716a/debug.html)
- [Browsing the internet](https://gisthost.github.io/?ddfaa18a70a4d7314fc8ada8fd2a716a/browser.html)
- [Playing nethack](https://gisthost.github.io/?ddfaa18a70a4d7314fc8ada8fd2a716a/nethack.html)

## Installation

### From source

```bash
cargo build --release
```

Binary path:

```text
target/release/gibberish
```

## Usage

If you want to share your shenanigans and see what the agent was doing,
it supports exporting a session to an html page:

```bash
gibberish --session-html session.html
```

And when you get bored of confirming input:

```bash
gibberish --yolo
```

## Configuration

Default configuration get install at `~/.config/gibberish/config.toml` - you can change settings there.

You'll need to put an OpenAI API key into that file or in your env as `OPENAI_API_KEY`.

> It could work with other models as it uses [rig](https://rig.rs), I just haven't
> gotten around to trying with any other models.

### Useful flags in config.toml

- `yolo = true`: Always run without approvals to send input to bash
- `llm.api_key = ...`: Set the API key here instead of in your env.

## REPL Commands

When you're in the interactive prompt, lines go to the agent unless you start with `:`.

- `:raw <escaped-bytes>`: Send bytes to the PTY, including control chars like `\x03`.
- `:snap`: Print the terminal screen.
- `:reset`: Restart the shell and wipe the agent chat history.
- `:help`: Print the command cheat sheet.
- `:quit` / `:q`: Quit.

## Development

Common commands:

```bash
just ci
just fmt
cargo generate-lockfile
```
