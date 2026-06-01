# Code Review Report: Daedalus AI Coding Agent Framework

**Date**: 2026-05-30  
**Scope**: ~57,000 LOC across ~150 Rust source files  
**Review Method**: 5 parallel subagent reviews covering all modules, followed by synthesis, verification, and deduplication  
**Overall Quality**: ⭐⭐⭐½ (Good)  
**Verdict**: **REQUEST CHANGES** — 6 High-severity issues requiring resolution before production deployment

---

## Executive Summary

Daedalus is a well-architected AI coding agent framework with clean module separation, thoughtful abstractions (RAII guards, middleware pipeline, memory backend factory), and strong security awareness (shell injection prevention, path traversal guards). The codebase demonstrates mature Rust patterns across most modules.

The review identified **22 issues** (6 High, 16 Medium) plus numerous minor observations and commendations. The primary areas of concern are:

1. **Crash resilience**: Multiple unwrap/expect calls in hot paths that can panic in production
2. **Security enforcement gaps**: Read-only mode is dead code, empty-input confirmation defaults to allow
3. **Reliability under degradation**: Timeout not enforced, circuit breakers with no recovery, streaming with no retry
4. **Resource management**: Memory exhaustion from unbounded file reads, orphaned git worktrees

---

## 🔴 High Severity (Must Fix)

### H1. Session memory lock panics on contention — `src/agent/session.rs:82-83`

**Problem**: `with_memory()` uses `tokio::sync::Mutex::try_lock().expect()`. `try_lock()` on a tokio Mutex fails whenever any async task holds the lock — not just under heavy contention. The `.expect()` crashes the entire process. The doc comment states "sync callers only run between async turns," but this invariant is not enforced by the type system.

```rust
// session.rs:78-84
pub fn with_memory<F, R>(&self, f: F) -> R {
    let mem = self.memory.try_lock()
        .expect("Memory lock should not be held during sync access");
    f(&**mem)
}
```

**Impact**: Any background task (recall_history middleware, tracing, concurrent tool callbacks) that happens to hold the memory lock crashes the process. Reachable in normal operation.

**Fix**: Convert to async (`self.memory.lock().await`) or restructure to avoid sync access patterns. Since this is `#[allow(dead_code)]`, evaluate whether it's still needed.

---

### H2. Turn timeout is observational, never enforced — `src/middleware/builtin/harness.rs:103-140`

**Problem**: `HarnessTurnMiddleware` claims to enforce a per-turn duration limit (`max_turn_duration_secs`), but only checks elapsed time **after** `next.run()` completes — it never wraps the call with `tokio::time::timeout`. A hung LLM call or stuck tool execution will block forever.

```rust
// harness.rs:112-113
let response = next.run(request).await?;  // ← No timeout wrapping!
let elapsed = start.elapsed();
if elapsed > max_duration {
    tracing::warn!(...);  // ← Only logs, never aborts
}
```

**Impact**: The agent can hang indefinitely on a stalled LLM API call or a deadlocked subprocess. The "timeout" configuration is misleading — it's purely for logging.

**Fix**: Wrap with `tokio::time::timeout`:
```rust
let response = tokio::time::timeout(max_duration, next.run(request))
    .await
    .map_err(|_| anyhow::anyhow!("Turn exceeded {}s timeout", max_duration))??;
```

---

### H3. `validate_read_only` is dead code — read-only mode completely unenforced — `src/tools/bash.rs:239`

**Problem**: The `validate_read_only` function exists with extensive allowlist/blocklist logic, but is annotated `#[allow(dead_code)]` and never called from any execution path. Both `execute()` and `execute_streaming()` run commands without checking read-only constraints.

**Impact**: Subagents configured for "plan/read-only mode" have **no actual enforcement** — they can execute arbitrary destructive commands (`rm -rf`, data exfiltration via `curl`). The dead code creates a false sense of security.

**Fix**: Either wire `validate_read_only` into the execution path when read-only mode is active, or remove it entirely to eliminate the security theater.

---

### H4. Read-only command allowlist permits arbitrary code execution — `src/tools/bash.rs:28-33`

**Problem**: Even if `validate_read_only` were wired in (H3), the allowlist includes full interpreters:
```rust
const READ_ONLY_ALLOWED_COMMANDS: &[&str] = &[
    "awk", "sed", "python", "node", "go", "cargo", "rustc", "git", ...
];
```
`python -c "import os; os.system('rm -rf /')"` passes validation. Same for `awk 'BEGIN {system("rm -rf /")}'`, `node -e "require('child_process').exec('rm -rf /')"`, etc.

**Impact**: The allowlist is trivially bypassed by anyone with basic scripting knowledge.

**Fix**: Remove full interpreters (`python`, `node`, `go`, `awk`, `sed`, `cargo`, `rustc`, `git`) from the allowlist. If scripting is needed, use a sandboxed execution environment (seccomp, container).

---

### H5. Empty input (Enter key) defaults to "Allow Once" on tool confirmation — `src/cli/repl/confirmation.rs:99`

**Problem**: Pressing Enter (empty input) on a tool confirmation prompt silently allows the tool call:
```rust
match trimmed.as_str() {
    "y" | "yes" | "" => UserDecision::AllowOnce,  // ← Empty string here!
```

**Impact**: An accidental Enter keypress on a `bash` tool with arbitrary execution approves it. This violates the principle of "default-deny for safety" and contradicts the function's own fallback behavior (unknown input and I/O errors already default to Deny at lines 110, 121).

**Fix**: Move `""` to the deny branch:
```rust
"n" | "no" | "d" | "deny" | "" => UserDecision::Deny,
```

---

### H6. Fire-and-forget thread leaks git worktrees — `src/subagent/isolation.rs:232`

**Problem**: `WorktreeGuard::drop()` spawns a detached `std::thread` for cleanup. If the tokio runtime exits before the git commands complete, the worktree directory and branch leak permanently. All errors from `git worktree remove` and `git branch -D` are silently discarded with `let _ =`.

```rust
let _ = std::thread::spawn(move || {
    let _ = std::process::Command::new("git")  // errors swallowed
        .args(["worktree", "remove", "--force"])
        .arg(&worktree_dir).output();
    let _ = std::process::Command::new("git")  // errors swallowed
        .args(["branch", "-D"])
        .arg(&branch_name).output();
});
```

**Impact**: Accumulation of orphaned git worktrees and branches over multiple subagent runs, eventually filling `/tmp` and cluttering the git repository.

**Fix**: Run cleanup synchronously in `drop()` (it's already called from `spawn_blocking` context) and at minimum log failures at `warn` level.

---

## 🟡 Medium Severity (Should Fix)

### M1. SSE stream UTF-8 corruption on chunk boundaries — `src/acp/http_client.rs:354`

`String::from_utf8_lossy` on arbitrary chunk boundaries replaces split multi-byte sequences with `�` (U+FFFD). No timeout on the read loop — a stalled remote server hangs forever.

**Fix**: Accumulate raw bytes in `Vec<u8>` and only convert complete lines to `str`.

---

### M2. Streaming requests have zero retry logic — `src/llm/provider.rs:261-299`

`send_request()` has robust retry (3 retries, exponential backoff), but `send_stream_request()` has none. Any transient 429/5xx on a streaming call is a hard error.

**Fix**: Add at least 1 retry for 429/5xx in the streaming path.

---

### M3. `read_file` loads entire file into memory — `src/tools/read_file.rs:53-57`

For a 500MB log file where the LLM requests lines 1-10, the entire file is loaded into memory. No size guard exists (unlike `edit_file.rs` which has a 10MB limit).

**Fix**: Stream-read with `AsyncBufReadExt::lines()`, skip to offset, collect only the requested lines. Add a file-size guard.

---

### M4. ChromaDB HTTP client silently falls back to no-timeout — `src/memory/mempalace/retriever.rs:78-81`

```rust
http_client: reqwest::Client::builder()
    .timeout(Duration::from_secs(30))
    .build()
    .unwrap_or_else(|_| reqwest::Client::new()),  // ← No timeout!
```

If TLS config or other builder error occurs, the fallback has no timeout and blocks indefinitely.

**Fix**: Apply timeout on the fallback client too, or propagate the error instead of silently falling back.

---

### M5. WAL errors silently swallowed — `src/memory/mempalace/wal.rs:77-91`

`WriteAheadLog::log()` catches all I/O errors but only logs them. Callers proceed with the write operation anyway, defeating the purpose of WAL.

**Fix**: Return `Result<(), WALError>` and check it at call sites.

---

### M6. Auto-compact circuit breaker has no recovery path — `src/memory/sliding_window/memory.rs:420-425`

After 3 failed LLM compaction calls (3 consecutive turns during an API outage), auto-compact is permanently disabled for the session. `compact_failure_count` is only reset on successful compact or session restart.

**Fix**: Add time-based or turn-based decay for the failure count.

---

### M7. SSE events silently dropped on channel full — `src/acp/transport.rs:427-429`

`try_send` drops events when the 64-capacity channel is full (slow consumer). Serialization errors silently produce empty-data events.

**Fix**: Use `send().await` for backpressure, or at minimum `warn!` on drop.

---

### M8. Inverted `has_tools` logic — `src/acp/agent_card.rs:193`

```rust
def.tools.as_ref().map(|t| !t.is_empty()).unwrap_or(true)  // Should be false
```

When `tools` is `None`, it reports tools as available. The opposite is correct — no tools config means no tools.

**Fix**: Change `unwrap_or(true)` to `unwrap_or(false)`.

---

### M9. TOCTOU race in `create_new_file` — `src/tools/edit_file.rs:147-149`

When `old_string` is empty, `create_new_file()` is called **before** `FileEditGuard` is acquired (the guard is acquired at line 152, but the function returns early at line 148). Two concurrent `edit_file` calls with `old_string: ""` on the same path can both pass the existence check, then one overwrites the other.

**Fix**: Acquire the guard before the empty-string early return.

---

### M10. No HTTP connection pooling across ACP instances — `src/acp/http_client.rs:65,122,164`

Every `RemoteAcpServer` creates a new `reqwest::Client`. Multiple parallel subagent calls to the same host create separate TCP connections instead of reusing a pool.

**Fix**: Accept a shared `Arc<Client>` or lazily initialize one at module level.

---

### M11. Tavily API key embedded in JSON request body — `src/tools/web_search/tavily.rs:22-23`

```rust
let body = serde_json::json!({
    "api_key": api_key,  // ← In body, captured by tracing
```

When tracing is enabled with `full_content: true`, the API key flows to file exporter, console, Langfuse, and OTLP — a cross-module credential leak.

**Fix**: Move the API key to an HTTP header (`Authorization: Bearer`).

---

### M12. Synchronous file I/O in async tracing context — `src/tracing/exporters/file.rs:68-93`

`write_line` uses synchronous `std::fs::OpenOptions` + `writeln!` inside `on_trace_end`. Under heavy tracing load, this blocks Tokio worker threads.

**Fix**: Use `tokio::fs` or `spawn_blocking`.

---

### M13. Sequential collector dispatch delays all exporters — `src/tracing/manager.rs:87-89`

All `notify_*` methods iterate collectors sequentially with `.await`. A slow collector (Langfuse) blocks all subsequent exporters.

**Fix**: Use `futures::future::join_all` for parallel fan-out.

---

### M14. ANSI escape sequences without TTY detection — `src/cli/repl/streaming.rs:154,163-169`

Cursor movement codes (`\x1B[nA`, `\x1B[2K`) are emitted unconditionally. When stdout is piped to a file, these produce garbage characters.

**Fix**: Check `std::io::stdout().is_terminal()` before emitting escape codes.

---

### M15. Mutex poison panics abort the process — `src/cli/repl/streaming.rs:95,107,119,129,142`

Multiple `.lock().expect("streaming state poisoned")` calls in the tool event callback hot path. If any thread panics while holding the mutex, all subsequent events crash the REPL.

**Fix**: Use `.lock().unwrap_or_else(|e| e.into_inner())` to recover from poison.

---

### M16. Trace export failures silently swallowed — `src/tracing/exporters/langfuse.rs:355-361`, `otel.rs:321-327`

Both exporters only `tracing::warn!` on failures. During network outages, trace data is lost without programmatic notification.

**Fix**: Return errors from `on_trace_end` or implement a retry buffer.

---

## 🔵 Low Severity (Consider Fixing)

- `src/skill/loader.rs:160` — Frontmatter parsing only handles `\n---\n`, not `\r\n`. SKILL.md files with CRLF silently fail to parse YAML frontmatter on Windows.
- `src/subagent/loader.rs:131` — Falls back to `"unknown"` when `file_stem()` returns None. Multiple files without stems collide on the same name.
- `src/middleware/builtin/context_engineering.rs:125` — Redundancy detection uses only first 200 chars for fingerprinting. Common tool output prefixes cause false positives.
- `src/middleware/builtin/context_engineering.rs:202-206` — Injects `ChatMessage::system` mid-conversation, which adapters process differently (Anthropic extracts to top-level system field).
- `src/llm/adapter/gemini.rs:322-329` — `uuid_v4_short()` is just a nanosecond timestamp, not a UUID. Concurrent calls in the same nanosecond produce identical IDs.
- `src/tracing/context.rs:496-497` — `rposition()` + `remove()` is O(n²) for the span stack. For traces with many nested spans, this adds measurable latency.
- `src/tools/grep_search.rs:37-44` — `candidate.is_file()` called synchronously inside `OnceLock::get_or_init`. If accessed before pre-init, blocks Tokio worker.
- `src/cli/commands.rs:71` — Flag parsing silently ignores typos (e.g., `--befor`) and treats remaining text as a compaction instruction with no error.
- `src/memory/persistence.rs:35` — `atomic_write` uses `path.with_extension("tmp")` without `fsync`, so durability depends on OS writeback behavior.

---

## 💚 Commendations

The codebase demonstrates excellent practices in many areas:

- **`src/hooks/executor.rs:44-91`** — Textbook-correct async child process management: spawns, reads stdout/stderr concurrently with `tokio::join!`, wraps with `timeout`, kills and reaps on timeout. Deadlock-free.

- **`src/subagent/isolation.rs:17-54`** — Comprehensive shell-injection prevention: validates agent names against alphanumeric-only with leading-dash rejection, validates lifecycle hooks against forbidden shell characters, executes directly without shell interpreter.

- **`src/tools/edit_file.rs:67-92`** — `FileEditGuard` RAII pattern with `Drop` implementation ensures edit locks are always released, even on panic. The `try_acquire`/`Drop` pattern is a model of correctness.

- **`src/tools/edit_file.rs:289-291`** — Line ending normalization (`\r\n` → `\n`) for matching is critical for correctness with cross-platform files.

- **`src/tools/bash.rs:16-21`** — DoS prevention: default 30s timeout, 300s max, 256KB output cap with truncation notification.

- **`src/tools/grep_search.rs:193-226`** — Correct use of `--` separator before user-supplied pattern prevents flag injection into ripgrep.

- **`src/config/loader.rs`** — Elegant two-phase config loading solving the chicken-and-egg problem of needing tracing to read config that configures tracing.

- **`src/agent/duplicate_detector.rs`** — Well-designed LLM-loop guard with thresholds, within-round deduplication, and human-readable warnings.

- **`src/llm/types.rs:136-155`** — `Debug` impl for `LlmConfig` redacts API key, showing only first 4 + last 4 characters.

- **`src/prompt/mod.rs:197-214`** — Excellent documentation of prompt section ordering with KV cache boundary optimization rationale.

- **`src/memory/factory.rs:17-109`** — Graceful fallback pattern: when embedding configuration is missing for backends that require it, falls back to SlidingWindow with clear error logging.

- No `unsafe` blocks (except in test), no `todo!()` markers, no `unreachable!()` in production code — indicates completeness.

---

## Cross-Module Issues

1. **Credential leak via tracing pipeline**: Tavily API key embedded in JSON body → captured by `SpanType::ToolCall.arguments` → flows to all enabled tracing exporters (file, console, Langfuse, OTLP). Affects `tools/web_search/tavily.rs` ↔ `tracing/types.rs` ↔ all exporters.

2. **Context anchor injection × Adapter mismatch**: `context_engineering.rs` injects `ChatMessage::system` mid-conversation, but Anthropic and Gemini adapters process system messages differently (extracting to top-level fields). Affects `middleware/builtin/context_engineering.rs` ↔ `llm/adapter/anthropic.rs` ↔ `llm/adapter/gemini.rs`.

3. **Mutex type inconsistency**: `session.rs` uses `tokio::sync::Mutex` with panic-on-contention, while `chat.rs:666` uses the same type with graceful fallback. `plan_tracker.rs` uses `std::sync::Mutex` but accesses it from async contexts — could accidentally block workers if the lock is ever held across an `.await`.

---

## Summary Statistics

| Severity | Count |
|----------|-------|
| 🔴 Critical | 0 |
| 🟠 High | 6 |
| 🟡 Medium | 16 |
| 🔵 Low | 9 |
| 💚 Commendations | 12 |

**Recommendation**: Address the 6 High-severity issues before production deployment. The Medium issues can be resolved incrementally. Overall, this is a well-constructed codebase that demonstrates solid Rust engineering practices.
