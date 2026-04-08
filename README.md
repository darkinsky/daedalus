# 🏛️ Daedalus

A terminal-based AI assistant built in Rust, inspired by [Claude Code](https://docs.anthropic.com/en/docs/claude-code). Daedalus provides an interactive REPL interface for multi-turn conversations with LLM providers, featuring session management, conversation memory, and rich terminal rendering.

## ✨ Features

- **Interactive REPL** — Claude Code-style terminal interface with slash commands
- **Multi-turn Conversations** — Full conversation history with configurable memory strategies
- **Provider Agnostic** — Pluggable LLM backend via trait abstraction (currently supports OpenAI-compatible APIs)
- **Session Management** — Create, switch, and track conversation sessions
- **Token Usage Tracking** — Monitor prompt/completion token consumption per session
- **Rich Terminal Output** — Markdown rendering, colored output, spinners, and styled prompts
- **Structured Logging** — Configurable file/stderr logging with rotation, JSON/pretty/compact formats
- **Modular Architecture** — Clean separation of concerns with trait-based abstractions

## 🏗️ Architecture

```
┌─────────────────────────────────────────────────────┐
│                      main.rs                        │
│         (config loading, wiring, entry point)       │
└──────────┬──────────────┬───────────────┬───────────┘
           │              │               │
     ┌─────▼─────┐  ┌────▼────┐   ┌──────▼──────┐
     │   cli/     │  │  agent/ │   │   logging   │
     │  (REPL,    │  │ (Agent  │   │  (tracing,  │
     │  commands, │  │  Mode,  │   │   rotation) │
     │  render)   │  │  Chat)  │   └─────────────┘
     └────────────┘  └────┬────┘
                          │
              ┌───────────┼───────────┐
              │           │           │
        ┌─────▼─────┐ ┌──▼───┐ ┌────▼─────┐
        │   llm/     │ │memory│ │ session  │
        │ (LlmApi,  │ │(Mem- │ │ (Session │
        │  GenAI     │ │ ory  │ │  state)  │
        │  provider) │ │trait)│ └──────────┘
        └────────────┘ └──────┘
```

### Module Overview

| Module | Description |
|--------|-------------|
| `cli/` | Terminal UI — REPL loop, slash command parsing, output rendering, token cost tracking |
| `agent/` | Agent abstraction — `AgentMode` trait and `ChatAgent` implementation |
| `llm/` | LLM provider abstraction — `LlmApi` trait, types, and GenAI-based provider |
| `memory/` | Conversation memory — `Memory` trait and sliding window implementation |
| `session` | Session state — ID, title, request counter, memory delegation |
| `config` | Configuration — Environment variable loading for agent settings |
| `logging` | Structured logging — Multi-format, file rotation, configurable output |

## 🚀 Getting Started

### Prerequisites

- **Rust** 2024 edition (1.85+)
- An **OpenAI-compatible API key** (OpenAI, Azure, or any compatible proxy)

### Installation

```bash
# Clone the repository
git clone <repo-url>
cd Daedalus

# Build the project
cargo build --release

# Run
cargo run --release
```

### Quick Start

```bash
# Set your API key
export OPENAI_API_KEY="your-api-key-here"

# (Optional) Use a custom model
export DAEDALUS_MODEL="gpt-4o"

# (Optional) Use a custom API endpoint (e.g., Azure, local proxy)
export OPENAI_BASE_URL="https://your-proxy.example.com/v1"

# Run Daedalus
cargo run --release
```

## ⚙️ Configuration

All configuration is done via environment variables:

### Agent Configuration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `OPENAI_API_KEY` | ✅ | — | API key for the LLM provider |
| `DAEDALUS_MODEL` | ❌ | `gpt-4o` | Model identifier to use |
| `OPENAI_BASE_URL` | ❌ | `https://api.openai.com/v1/` | Custom API base URL |
| `DAEDALUS_SYSTEM_PROMPT` | ❌ | Built-in prompt | Custom system prompt for the assistant |

### Logging Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `RUST_LOG` | `daedalus=debug` | Log filter directive (standard `tracing` format) |
| `DAEDALUS_LOG_FORMAT` | `pretty` | Stderr format: `pretty`, `compact`, `json`, `full` |
| `DAEDALUS_LOG_DIR` | *(disabled)* | Directory for rolling log files (enables file logging) |
| `DAEDALUS_LOG_FILE_PREFIX` | `daedalus` | Log file name prefix |
| `DAEDALUS_LOG_ROTATION` | `daily` | Rotation policy: `minutely`, `hourly`, `daily`, `never` |
| `DAEDALUS_LOG_FILE_FORMAT` | `json` | File log format: `pretty`, `compact`, `json`, `full` |
| `DAEDALUS_LOG_FILE` | `false` | Show source file in logs |
| `DAEDALUS_LOG_LINE` | `false` | Show line numbers in logs |
| `DAEDALUS_LOG_TARGET` | `true` | Show target module path |
| `DAEDALUS_LOG_THREAD_NAMES` | `false` | Show thread names |
| `DAEDALUS_LOG_ANSI` | `true` | Use ANSI color codes (stderr only) |

**Example** — Enable file logging with hourly rotation:

```bash
export DAEDALUS_LOG_DIR="./logs"
export DAEDALUS_LOG_ROTATION="hourly"
export DAEDALUS_LOG_FILE_FORMAT="json"
cargo run
```

## 💬 Usage

Once running, you'll see the startup banner:

```
🏛️ Daedalus  v0.1.0

  Model:    gpt-4o  (GenAI)
  Mode:     chat
  Session:  Session 2026-04-08 11:00:00 (a1b2c3d4)

  Type /help for available commands.

>
```

Type a message and press Enter to chat. The assistant's response will be rendered with terminal markdown support.

### Slash Commands

| Command | Aliases | Description |
|---------|---------|-------------|
| `/help` | `/h`, `/?` | Show available commands |
| `/new` | `/compact` | Start a new conversation session (clears history) |
| `/clear` | — | Clear the screen (keeps conversation history) |
| `/cost` | — | Show token usage for the current session |
| `/model` | — | Show current model and provider information |
| `/exit` | `/quit` | Exit the application |

You can also type `quit` or `exit` (without slash) to exit.

## 📁 Project Structure

```
src/
├── main.rs                  # Entry point: config loading, wiring, startup
├── config.rs                # AgentConfig — env var loading
├── session.rs               # Session — ID, title, memory delegation
├── logging.rs               # Structured logging with rotation support
│
├── agent/
│   ├── mod.rs               # AgentMode trait definition
│   └── chat.rs              # ChatAgent — multi-turn chat implementation
│
├── cli/
│   ├── mod.rs               # Module entry, exports run_interactive()
│   ├── repl.rs              # Main REPL loop
│   ├── commands.rs          # Slash command parsing and definitions
│   ├── render.rs            # Terminal output rendering (banner, help, etc.)
│   └── cost.rs              # SessionCost — token usage tracking
│
├── llm/
│   ├── mod.rs               # LlmApi trait + provider factory
│   ├── types.rs             # ChatMessage, ChatResponse, LlmConfig, etc.
│   └── genai_provider.rs    # GenAI-based LLM provider implementation
│
└── memory/
    ├── mod.rs               # Memory trait definition
    └── sliding_window.rs    # SlidingWindowMemory (unlimited / bounded)
```

## 🧩 Design Principles

- **Trait-based Abstraction** — Core interfaces (`AgentMode`, `LlmApi`, `Memory`) are defined as traits, enabling easy extension and testing
- **Dependency Injection** — `ChatAgent` receives its LLM provider and memory factory as injected dependencies, not hard-coded implementations
- **Single Responsibility** — Each module has a clear, focused purpose (e.g., `cli/render.rs` only handles output, `cli/commands.rs` only handles parsing)
- **High Cohesion, Low Coupling** — Modules communicate through well-defined trait interfaces; the CLI knows nothing about LLM internals

## 🛠️ Development

```bash
# Run tests
cargo test

# Build in debug mode
cargo build

# Run with debug logging to stderr
RUST_LOG=daedalus=debug cargo run

# Run with file logging
DAEDALUS_LOG_DIR=./logs cargo run

# Check for warnings
cargo clippy
```

## 📄 License

This project is private and not yet licensed for public distribution.
