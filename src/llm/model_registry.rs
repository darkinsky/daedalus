//! Built-in model registry for known context window sizes.
//!
//! This module provides a lookup function that maps model identifiers to
//! their known maximum context window size (in tokens). This allows the
//! system to automatically configure truncation and compression budgets
//! without requiring manual configuration for common models.
//!
//! Users can override the registry value via `llm.context_window` in config.
//!
//! Last updated: 2026-05-09

/// Default context window size (in tokens) when the model is not recognized.
pub const DEFAULT_CONTEXT_WINDOW: usize = 128_000;

/// Look up the known context window size for a model by its identifier.
///
/// Returns `Some(tokens)` if the model is recognized, `None` otherwise.
/// The caller should fall back to `DEFAULT_CONTEXT_WINDOW` when `None`.
///
/// # Matching strategy
///
/// Uses case-insensitive substring matching against known model families.
/// This is intentionally generous to handle version suffixes, date stamps,
/// and provider-specific naming conventions (e.g., "claude-opus-4-7").
pub fn model_context_window(model: &str) -> Option<usize> {
    let m = model.to_lowercase();

    // ── Anthropic Claude ──
    if m.contains("claude") {
        // Claude 4.7 (Opus 4.7, Sonnet 4.7) — 1M context (2026-04)
        if m.contains("4.7") || m.contains("4-7") {
            return Some(1_000_000);
        }
        // Claude 4.6 (Opus 4.6, Sonnet 4.6) — 1M context (2026-02/03)
        if m.contains("4.6") || m.contains("4-6") {
            return Some(1_000_000);
        }
        // Claude Sonnet 4 — 1M context (expanded 2025-08 from 200K)
        if m.contains("sonnet-4") || m.contains("sonnet4") {
            return Some(1_000_000);
        }
        // Claude Opus 4 — 200K (original)
        if m.contains("opus-4") || m.contains("opus4") {
            return Some(200_000);
        }
        // Claude 4 (unspecified variant) — default to 200K (conservative)
        // This must come AFTER the specific sonnet-4/opus-4 checks above,
        // otherwise it would swallow all Claude 4 variants.
        if m.contains("claude-4") {
            return Some(200_000);
        }
        // Claude 3.7 Sonnet — 200K context (2025-02)
        if m.contains("3-7") || m.contains("3.7") {
            return Some(200_000);
        }
        // Claude 3.5 family — 200K context
        if m.contains("3-5") || m.contains("3.5") {
            return Some(200_000);
        }
        // Claude 3 family (Opus, Sonnet, Haiku) — 200K context
        if m.contains("claude-3") {
            return Some(200_000);
        }
        // Default Claude — 200K
        return Some(200_000);
    }

    // ── OpenAI o-series (reasoning models) ──
    if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        return Some(200_000);
    }

    // ── OpenAI GPT-5.x family (2026) — 1M context ──
    if m.contains("gpt-5") {
        return Some(1_000_000);
    }

    // ── OpenAI GPT-4.1 family (2025-04) — 1M context ──
    if m.contains("gpt-4.1") || m.contains("gpt-4-1") {
        return Some(1_000_000);
    }

    // ── OpenAI GPT-4.5 (2025-02) — 128K context ──
    if m.contains("gpt-4.5") || m.contains("gpt-4-5") {
        return Some(128_000);
    }

    // ── OpenAI GPT-4 family ──
    if m.contains("gpt-4") {
        // GPT-4o, GPT-4o-mini — 128K context
        if m.contains("4o") {
            return Some(128_000);
        }
        // GPT-4-turbo — 128K context
        if m.contains("turbo") {
            return Some(128_000);
        }
        // GPT-4 (original) — 8K context
        return Some(8_192);
    }

    // ── OpenAI GPT-3.5 ──
    if m.contains("gpt-3.5") {
        if m.contains("16k") {
            return Some(16_384);
        }
        return Some(16_384);
    }

    // ── DeepSeek ──
    if m.contains("deepseek") {
        // DeepSeek-V4 (Pro/Flash) — 1M context (2026-04)
        if m.contains("v4") {
            return Some(1_000_000);
        }
        // DeepSeek-V3 — 128K context (2024-12)
        if m.contains("v3") {
            return Some(128_000);
        }
        // DeepSeek-R1 — 128K context (2025-01)
        if m.contains("r1") {
            return Some(128_000);
        }
        // deepseek-chat / deepseek-reasoner (legacy aliases) — 128K
        return Some(128_000);
    }

    // ── Google Gemini ──
    if m.contains("gemini") {
        // Gemini 3.1 (Pro/Flash/Flash-Lite) — 1M context (2026-02/03)
        if m.contains("3.1") || m.contains("3-1") {
            return Some(1_000_000);
        }
        // Gemini 3 (Pro/Flash) — 1M context (2025-11/12)
        if m.contains("3.0") || m.contains("3-0") || m.contains("gemini-3") {
            return Some(1_000_000);
        }
        // Gemini 2.5 (Pro/Flash/Flash-Lite) — 1M context (2025-03)
        if m.contains("2.5") || m.contains("2-5") {
            return Some(1_048_576);
        }
        // Gemini 2.0 (Flash) — 1M context (2024-12)
        if m.contains("2.0") || m.contains("2-0") {
            return Some(1_048_576);
        }
        // Gemini 1.5 (Pro/Flash) — 1M context (2024-05)
        if m.contains("1.5") || m.contains("1-5") {
            return Some(1_048_576);
        }
        // Default Gemini (latest) — 1M
        return Some(1_000_000);
    }

    // ── 智谱 GLM ──
    if m.contains("glm") {
        // GLM-5.1 — 203K context (2026-04)
        if m.contains("5.1") || m.contains("5-1") {
            return Some(203_000);
        }
        // GLM-5 — 200K context (2026-02)
        if m.contains("glm-5") || m.contains("glm5") {
            return Some(200_000);
        }
        // GLM-4 with 1M variant
        if m.contains("1m") {
            return Some(1_000_000);
        }
        // GLM-4-Plus / GLM-4 — 128K context (2024-01)
        if m.contains("glm-4") || m.contains("glm4") {
            return Some(128_000);
        }
        // Default GLM
        return Some(128_000);
    }

    // ── MiniMax ──
    if m.contains("minimax") || m.contains("abab") {
        // MiniMax-Text-01 / MiniMax-VL-01 — 4M context (2025-01)
        if m.contains("text-01") || m.contains("vl-01") {
            return Some(4_000_000);
        }
        // MiniMax-M1 — 1M context (2025-06)
        if m.contains("m1") || m.contains("m-1") {
            return Some(1_000_000);
        }
        // abab7 — 1M context
        if m.contains("abab7") {
            return Some(1_000_000);
        }
        // abab6.5 — 200K context
        if m.contains("abab6") {
            return Some(200_000);
        }
        // Default MiniMax
        return Some(1_000_000);
    }

    // ── Qwen ──
    if m.contains("qwen") {
        // Qwen-Long — 10M context
        if m.contains("long") {
            return Some(10_000_000);
        }
        // Qwen-Max / Qwen-2.5 — 128K context
        if m.contains("max") || m.contains("2.5") || m.contains("2-5") {
            return Some(128_000);
        }
        // Qwen default
        return Some(128_000);
    }

    // ── Mistral / Mixtral ──
    if m.contains("mistral") || m.contains("mixtral") {
        // Mistral Large (2025) — 128K context
        if m.contains("large") {
            return Some(128_000);
        }
        // Default Mistral — 32K
        return Some(32_000);
    }

    // ── Meta Llama ──
    if m.contains("llama") {
        // Llama 4 — 10M context (2025-04)
        if m.contains("4") {
            return Some(10_000_000);
        }
        // Llama 3.x — 128K context
        if m.contains("3.") || m.contains("3-") {
            return Some(128_000);
        }
        // Llama 2 — 4K context
        return Some(4_096);
    }

    // ── Moonshot / Kimi ──
    if m.contains("moonshot") || m.contains("kimi") {
        // Kimi K2.6 — 256K context (2026-04, 1.1T MoE, 32B active)
        if m.contains("k2.6") || m.contains("k2-6") {
            return Some(256_000);
        }
        // Kimi K2.5 — 256K context (2026-01, 1T MoE, multimodal)
        if m.contains("k2.5") || m.contains("k2-5") {
            return Some(256_000);
        }
        // Kimi K2 — 256K context (2025, upgraded from 128K)
        if m.contains("k2") {
            return Some(256_000);
        }
        // Kimi K1.5 — 128K context (2025-01)
        if m.contains("k1.5") || m.contains("k1-5") {
            return Some(128_000);
        }
        // moonshot-v1-128k
        if m.contains("128k") {
            return Some(128_000);
        }
        // moonshot-v1-32k
        if m.contains("32k") {
            return Some(32_000);
        }
        // Default Moonshot/Kimi — 256K (latest default)
        return Some(256_000);
    }

    // Model not recognized
    None
}

/// Resolve the effective context window size for a model.
///
/// Priority:
/// 1. Explicit `override_value` from config (`llm.context_window`)
/// 2. Built-in registry lookup by model name
/// 3. `DEFAULT_CONTEXT_WINDOW` (128K)
pub fn resolve_context_window(model: &str, override_value: Option<usize>) -> usize {
    if let Some(explicit) = override_value {
        return explicit;
    }
    model_context_window(model).unwrap_or(DEFAULT_CONTEXT_WINDOW)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_models() {
        // Claude 4.7 — 1M
        assert_eq!(model_context_window("claude-opus-4-7"), Some(1_000_000));
        assert_eq!(model_context_window("claude-sonnet-4-7"), Some(1_000_000));
        assert_eq!(model_context_window("claude-4.7-opus"), Some(1_000_000));
        // Claude 4.6 — 1M
        assert_eq!(model_context_window("claude-opus-4-6"), Some(1_000_000));
        assert_eq!(model_context_window("claude-sonnet-4-6"), Some(1_000_000));
        // Claude Sonnet 4 — 1M (expanded 2025-08)
        assert_eq!(model_context_window("claude-sonnet-4-20250514"), Some(1_000_000));
        // Claude Opus 4 — 200K
        assert_eq!(model_context_window("claude-opus-4-20250514"), Some(200_000));
        // Claude 4 (unspecified variant) — 200K (conservative fallback)
        assert_eq!(model_context_window("claude-4"), Some(200_000));
        assert_eq!(model_context_window("claude-4-20250601"), Some(200_000));
        // Claude 3.7 — 200K
        assert_eq!(model_context_window("claude-3-7-sonnet-20250219"), Some(200_000));
        // Claude 3.5 — 200K
        assert_eq!(model_context_window("claude-3-5-sonnet-20241022"), Some(200_000));
        // Claude 3 — 200K
        assert_eq!(model_context_window("claude-3-opus-20240229"), Some(200_000));
    }

    #[test]
    fn test_openai_models() {
        // GPT-5.5 — 1M
        assert_eq!(model_context_window("gpt-5.5"), Some(1_000_000));
        assert_eq!(model_context_window("gpt-5.5-pro"), Some(1_000_000));
        // GPT-5.4 — 1M
        assert_eq!(model_context_window("gpt-5.4"), Some(1_000_000));
        assert_eq!(model_context_window("gpt-5.4-2026-03-05"), Some(1_000_000));
        // GPT-4.1 — 1M
        assert_eq!(model_context_window("gpt-4.1"), Some(1_000_000));
        assert_eq!(model_context_window("gpt-4.1-mini"), Some(1_000_000));
        assert_eq!(model_context_window("gpt-4.1-nano"), Some(1_000_000));
        // GPT-4.5 — 128K
        assert_eq!(model_context_window("gpt-4.5-preview"), Some(128_000));
        // GPT-4o — 128K
        assert_eq!(model_context_window("gpt-4o"), Some(128_000));
        assert_eq!(model_context_window("gpt-4o-mini"), Some(128_000));
        // GPT-4-turbo — 128K
        assert_eq!(model_context_window("gpt-4-turbo"), Some(128_000));
        // GPT-4 (original) — 8K
        assert_eq!(model_context_window("gpt-4"), Some(8_192));
        // o-series — 200K
        assert_eq!(model_context_window("o1-preview"), Some(200_000));
        assert_eq!(model_context_window("o3-mini"), Some(200_000));
        assert_eq!(model_context_window("o4-mini"), Some(200_000));
    }

    #[test]
    fn test_deepseek_models() {
        // DeepSeek V4 — 1M
        assert_eq!(model_context_window("deepseek-v4-pro"), Some(1_000_000));
        assert_eq!(model_context_window("deepseek-v4-flash"), Some(1_000_000));
        // DeepSeek V3 — 128K
        assert_eq!(model_context_window("deepseek-v3"), Some(128_000));
        // DeepSeek R1 — 128K
        assert_eq!(model_context_window("deepseek-r1"), Some(128_000));
        // Legacy aliases — 128K
        assert_eq!(model_context_window("deepseek-chat"), Some(128_000));
        assert_eq!(model_context_window("deepseek-reasoner"), Some(128_000));
    }

    #[test]
    fn test_gemini_models() {
        // Gemini 3.1 — 1M
        assert_eq!(model_context_window("gemini-3.1-pro"), Some(1_000_000));
        assert_eq!(model_context_window("gemini-3.1-flash"), Some(1_000_000));
        assert_eq!(model_context_window("gemini-3.1-flash-lite"), Some(1_000_000));
        // Gemini 3 — 1M
        assert_eq!(model_context_window("gemini-3-pro"), Some(1_000_000));
        assert_eq!(model_context_window("gemini-3-flash"), Some(1_000_000));
        // Gemini 2.5 — 1M (1048576)
        assert_eq!(model_context_window("gemini-2.5-pro"), Some(1_048_576));
        assert_eq!(model_context_window("gemini-2.5-flash"), Some(1_048_576));
        // Gemini 2.0 — 1M
        assert_eq!(model_context_window("gemini-2.0-flash"), Some(1_048_576));
        // Gemini 1.5 — 1M
        assert_eq!(model_context_window("gemini-1.5-pro"), Some(1_048_576));
    }

    #[test]
    fn test_glm_models() {
        // GLM-5.1 — 203K
        assert_eq!(model_context_window("glm-5.1"), Some(203_000));
        // GLM-5 — 200K
        assert_eq!(model_context_window("glm-5"), Some(200_000));
        // GLM-4 — 128K
        assert_eq!(model_context_window("glm-4-plus"), Some(128_000));
        assert_eq!(model_context_window("glm-4"), Some(128_000));
        // GLM-4-1M variant
        assert_eq!(model_context_window("glm-4-9b-chat-1m"), Some(1_000_000));
    }

    #[test]
    fn test_minimax_models() {
        // MiniMax-Text-01 — 4M
        assert_eq!(model_context_window("MiniMax-Text-01"), Some(4_000_000));
        // MiniMax-M1 — 1M
        assert_eq!(model_context_window("minimax-m1"), Some(1_000_000));
        // abab7 — 1M
        assert_eq!(model_context_window("abab7-chat"), Some(1_000_000));
        // abab6.5 — 200K
        assert_eq!(model_context_window("abab6.5s-chat"), Some(200_000));
    }

    #[test]
    fn test_kimi_models() {
        // Kimi K2.6 — 256K
        assert_eq!(model_context_window("kimi-k2.6"), Some(256_000));
        // Kimi K2.5 — 256K
        assert_eq!(model_context_window("kimi-k2.5"), Some(256_000));
        // Kimi K2 — 256K
        assert_eq!(model_context_window("kimi-k2"), Some(256_000));
        // Kimi K1.5 — 128K
        assert_eq!(model_context_window("kimi-k1.5"), Some(128_000));
        // moonshot-v1-128k — 128K
        assert_eq!(model_context_window("moonshot-v1-128k"), Some(128_000));
        // moonshot-v1-32k — 32K
        assert_eq!(model_context_window("moonshot-v1-32k"), Some(32_000));
    }

    #[test]
    fn test_unknown_model() {
        assert_eq!(model_context_window("some-unknown-model"), None);
    }

    #[test]
    fn test_resolve_with_override() {
        assert_eq!(resolve_context_window("gpt-4o", Some(256_000)), 256_000);
        assert_eq!(resolve_context_window("gpt-4o", None), 128_000);
        assert_eq!(resolve_context_window("unknown", None), DEFAULT_CONTEXT_WINDOW);
    }
}
