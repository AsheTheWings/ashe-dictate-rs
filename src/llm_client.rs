use crate::config::AppConfig;
use anyhow::{Context, Result, anyhow};
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::chat::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequestArgs,
};

const SYSTEM_PROMPT: &str = "You rewrite dictated speech into clear, coherent text. Correct grammar, punctuation, casing, and formatting. Remove filler words, false starts, repeated phrases, and disfluencies. Preserve the speaker's intent and meaning. Do not add facts. Return only the final rewritten text.";

pub async fn polish_transcript(config: AppConfig, transcript: String) -> Result<String> {
    let transcript = transcript.trim();
    if transcript.is_empty() {
        return Ok(String::new());
    }

    let openai_config = OpenAIConfig::new()
        .with_api_key(config.fireworks_api_key.clone())
        .with_api_base(config.fireworks_api_base.clone());
    let client = Client::with_config(openai_config);
    let request = CreateChatCompletionRequestArgs::default()
        .model(config.fireworks_model.clone())
        .messages(vec![
            ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessageArgs::default()
                    .content(SYSTEM_PROMPT)
                    .build()?,
            ),
            ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessageArgs::default()
                    .content(transcript.to_string())
                    .build()?,
            ),
        ])
        .temperature(config.llm_temperature)
        .max_completion_tokens(config.llm_max_tokens)
        .build()?;

    let response = client
        .chat()
        .create(request)
        .await
        .context("Fireworks chat completion failed")?;
    let text = response
        .choices
        .first()
        .and_then(|choice| choice.message.content.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("Fireworks returned an empty response"))?;
    Ok(text)
}
