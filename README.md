# vclaw

Voice control for [Claude Code](https://docs.anthropic.com/en/docs/claude-code). Speak commands, vclaw types them into Claude Code running in tmux, and speaks the results back.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/MadHouseLabs/vclaw/master/install.sh | sh
```

Requires [tmux](https://github.com/tmux/tmux) and [Claude Code](https://docs.anthropic.com/en/docs/claude-code).

## Usage

```sh
vclaw              # Start in current directory
vclaw attach       # Reattach to existing session
vclaw auth         # Authenticate with Anthropic
```

### Key bindings

| Key | Action |
|-----|--------|
| `F12` | Push-to-talk / interrupt |
| `Alt-M` | Toggle mute |

## Build from source

```sh
brew install cmake              # macOS
# sudo apt install cmake libasound2-dev   # Linux

cargo install --path .
```

The OAuth client ID is injected at compile time via `ANTHROPIC_OAUTH_CLIENT_ID` env var. Release binaries get it from a GitHub Actions secret.

## Architecture

See [docs/architecture.md](docs/architecture.md).

## License

MIT
