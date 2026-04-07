use rig::providers::openai::completion::CompletionResponse;

fn main() {
    let json = r#"{"id":"chatcmpl-DRxFZ05IyvVr6zrchw6BOULPu63Nc","object":"chat.completion","created":1775554985,"model":"gpt-4o-2024-05-13","choices":[{"index":0,"message":{"role":"assistant","content":"Hello! How can I assist you today?"},"finish_reason":"stop"}],"usage":{"prompt_tokens":8,"completion_tokens":10,"total_tokens":18,"completion_tokens_details":{"accepted_prediction_tokens":0,"audio_tokens":0,"reasoning_tokens":0,"rejected_prediction_tokens":0},"prompt_tokens_details":{"audio_tokens":0,"cached_tokens":0,"cache_read_tokens":0}},"venusMarker":{"spanId":"39d9e0aafba591bb"},"system_fingerprint":"fp_6b65be4bba"}"#;

    match serde_json::from_str::<CompletionResponse>(json) {
        Ok(resp) => println!("OK: {:?}", resp),
        Err(e) => println!("Deser CompletionResponse ERROR: {}", e),
    }

    // Also test ApiResponse untagged
    println!("\n--- Testing with serde_json::Value ---");
    let val: serde_json::Value = serde_json::from_str(json).unwrap();
    println!("Raw JSON parsed OK: {}", serde_json::to_string_pretty(&val).unwrap());
}
