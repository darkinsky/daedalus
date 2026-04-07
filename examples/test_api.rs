use anyhow::Result;
use rig::completion::{Prompt, ToolDefinition};
use rig::prelude::CompletionClient;
use rig::providers::openai;
use rig::tool::Tool;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct CalcArgs {
    expression: String,
}

#[derive(Debug, thiserror::Error)]
#[error("Calc error")]
struct CalcError;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct Calculator;

impl Tool for Calculator {
    const NAME: &'static str = "calculator";
    type Args = CalcArgs;
    type Output = String;
    type Error = CalcError;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "calculator".to_string(),
            description: "Evaluate a math expression".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "expression": {
                        "type": "string",
                        "description": "The math expression"
                    }
                },
                "required": ["expression"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        Ok(format!("Result: {}", args.expression))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("rig=trace")
        .init();

    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY not set");
    let base_url = std::env::var("OPENAI_BASE_URL").ok();

    let mut builder = openai::CompletionsClient::builder()
        .api_key(&api_key);

    if let Some(ref url) = base_url {
        println!("Using base URL: {}", url);
        builder = builder.base_url(url);
    }

    let client = builder.build()?;

    let agent = client
        .agent("gpt-4o")
        .preamble("You are a helpful assistant.")
        .tool(Calculator)
        .build();

    println!("Sending prompt with tool...");
    match agent.prompt("hello").await {
        Ok(response) => println!("Response: {}", response),
        Err(e) => println!("Error: {:?}", e),
    }

    Ok(())
}
