//! ACP (Agent Communication Protocol) — standardized inter-agent communication.
//!
//! ACP provides a protocol layer for agents to discover, communicate with,
//! and delegate tasks to each other. It is inspired by Google's A2A protocol
//! but tailored for Daedalus's trait-based architecture.
//!
//! ## Phase 1: Core Protocol Foundation
//!
//! - **types**: Protocol message types (requests, responses, events, errors)
//! - **agent_card**: Agent capability descriptors (skills, endpoints, metadata)
//! - **server**: `AcpServer` trait — the standard interface for receiving requests
//! - **client**: `AcpClient` — sends requests to local or remote agents
//!
//! ## Phase 2: HTTP/SSE Transport
//!
//! - **transport**: HTTP server (axum) exposing `AcpServer` instances over REST + SSE
//! - **http_client**: `RemoteAcpServer` — HTTP client that implements `AcpServer`
//!   for transparent remote agent access
//!
//! ## Phase 3: Agent Integration
//!
//! - **tool**: `AcpTool` — `BuiltinTool` adapter exposing ACP agents to the LLM
//! - **AcpConfig** — YAML configuration for declaring ACP agents
//! - **init_acp_client** — Bootstrap helper for connecting to configured agents
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────┐     AcpMessage      ┌─────────────┐
//! │  AcpClient   │ ──────────────────► │  AcpServer   │
//! │  (requester) │                     │  (provider)  │
//! │              │ ◄────────────────── │              │
//! └─────────────┘     AcpResponse      └─────────────┘
//!        │                                    │
//!        ▼                                    ▼
//!   AgentCard                            AgentCard
//!   (discovery)                          (self-describe)
//!
//!   ┌──────────────────── Phase 2 Transport ───────────────────┐
//!   │                                                          │
//!   │  Local Agent ◄──► AcpClient ◄──► RemoteAcpServer        │
//!   │                                       │                  │
//!   │                                  HTTP/SSE                │
//!   │                                       │                  │
//!   │                                  AcpTransport            │
//!   │                                  (axum server)           │
//!   │                                       │                  │
//!   │                                  AcpServer impl          │
//!   └──────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Design Principles
//!
//! 1. **Trait-first**: `AcpServer` is a trait, allowing any agent to become
//!    a protocol participant by implementing it.
//! 2. **Transport-agnostic**: The protocol defines messages and semantics,
//!    not wire format. Phase 2 adds HTTP/SSE transport.
//! 3. **Backward-compatible**: Existing `SubagentRunner` can be wrapped as
//!    an `AcpServer` without breaking changes.
//! 4. **Streaming-ready**: `TaskEvent` enum supports incremental results
//!    delivered via SSE in Phase 2.
//! 5. **Location-transparent**: `AcpClient` treats local and remote agents
//!    identically — both implement the `AcpServer` trait.

// Phase 1: Core protocol foundation
// These modules define the ACP protocol surface. Many types/methods are
// public API intended for external consumers (e.g., agents implementing
// AcpServer) and may not be used internally yet.
#[allow(dead_code)]
pub mod types;
#[allow(dead_code)]
pub mod agent_card;
#[allow(dead_code)]
pub mod server;
#[allow(dead_code)]
pub mod client;

// Phase 2: HTTP/SSE transport
// These modules (~1500 lines) are not yet integrated into the main agent flow.
// They are tightly coupled (http_client depends on transport types), so they
// must be gated together. TODO: gate behind `acp-server` feature flag once
// Phase 2 is integrated or the type dependency is broken.
#[allow(dead_code)]
pub mod transport;
#[allow(dead_code)]
pub mod http_client;

// Phase 3: Agent integration (BuiltinTool + config + bootstrap)
pub mod tool;

// Re-export types actually used by other modules (main.rs, config/loader.rs).
// Phase 3 re-exports — used by main.rs for bootstrap and config/loader.rs for deserialization.
pub use tool::{AcpConfig, init_acp_client, build_acp_tool};
