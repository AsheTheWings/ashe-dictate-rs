use crate::config::AppConfig;
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

const SYSTEM_PROMPT: &str = "Rewrite dictated speech into clear text as fast as possible. Do not reason. Do not explain. Correct grammar, punctuation, casing, and formatting. Remove filler words, false starts, repeated phrases, and disfluencies. Preserve intent and meaning. Do not add facts. Return only the final rewritten text.";

/// Polish a dictated transcript through the tera gateway's Open Responses API.
///
/// The prompt and the target text are sent together as a single `role: "user"` message,
/// with the dictated text (and any selected context) wrapped in XML-style tags to focus
/// the model's attention. The assistant `output_text` is read back out.
pub async fn polish_transcript(
    config: AppConfig,
    transcript: String,
    context: Option<String>,
) -> Result<String> {
    let transcript = transcript.trim();
    if transcript.is_empty() {
        return Ok(String::new());
    }

    request_response(
        &config,
        polish_prompt(transcript, context.as_deref()),
        config.llm_temperature,
    )
    .await
}

const GRAMMAR_PROMPT: &str = "Fix grammar, spelling, punctuation, and casing in the user's text. Preserve the original meaning, tone, intent, and language. Do not add or remove information. Do not explain. Return only the corrected text.";
const QUESTION_PROMPT: &str = "You are a helpful assistant. Answer the user's question directly and concisely. Do not restate the question. Reply in the same language as the question. Return only the answer.";
const QUESTION_TEMPERATURE: f32 = 0.7;

/// Correct grammar/spelling/punctuation in selected text and return only the rewrite.
///
/// Used by the `Win+Shift+G` flow, where the result replaces the original selection.
pub async fn fix_grammar(config: AppConfig, text: String) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(String::new());
    }
    let message = format!("{GRAMMAR_PROMPT}\n\n{}", tagged("text", text));
    request_response(&config, message, config.llm_temperature).await
}

/// Answer the selected text as a general question and return only the answer.
///
/// Used by the `Win+Shift+Q` flow, where the answer is appended after the selection.
pub async fn answer_question(config: AppConfig, question: String) -> Result<String> {
    let question = question.trim();
    if question.is_empty() {
        return Ok(String::new());
    }
    let message = format!("{QUESTION_PROMPT}\n\n{}", tagged("question", question));
    request_response(&config, message, QUESTION_TEMPERATURE).await
}

/// Shared Open Responses (`/v1/responses`) call: sends a single `role: "user"` message
/// (prompt + tag-wrapped target text) and reads the assistant `output_text` back out.
/// Reasoning is disabled for low-latency text actions.
async fn request_response(
    config: &AppConfig,
    user_content: String,
    temperature: f32,
) -> Result<String> {
    let endpoint = format!("{}/responses", config.tera_api_base.trim_end_matches('/'));
    let body = json!({
        "model": config.tera_model,
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": user_content,
            }
        ],
        "stream": false,
        "temperature": temperature,
        "reasoning": { "effort": "none" },
    });

    let client = reqwest::Client::new();
    let response = client
        .post(&endpoint)
        .bearer_auth(&config.tera_api_key)
        .json(&body)
        .send()
        .await
        .context("tera responses request failed")?;

    let status = response.status();
    let payload: Value = response
        .json()
        .await
        .context("tera responses returned a non-JSON body")?;

    if !status.is_success() {
        let message = payload
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| payload.get("detail").and_then(Value::as_str))
            .unwrap_or("unknown error");
        return Err(anyhow!("tera responses failed ({status}): {message}"));
    }

    let text = extract_output_text(&payload);
    let text = text.trim();
    if text.is_empty() {
        return Err(anyhow!("tera returned an empty response"));
    }
    Ok(text.to_string())
}

/// Collect the assistant-visible text from a Responses `output` array. Reasoning items
/// (which carry no `output_text`) are skipped; refusals are surfaced as their text.
fn extract_output_text(payload: &Value) -> String {
    let mut out = String::new();
    let Some(items) = payload.get("output").and_then(Value::as_array) else {
        return out;
    };
    for item in items {
        if item.get("type").and_then(Value::as_str).unwrap_or("message") != "message" {
            continue;
        }
        match item.get("content") {
            Some(Value::String(content)) => out.push_str(content),
            Some(Value::Array(parts)) => {
                for part in parts {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") | Some("input_text") => {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                out.push_str(text);
                            }
                        }
                        Some("refusal") => {
                            if let Some(text) = part.get("refusal").and_then(Value::as_str) {
                                out.push_str(text);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn polish_prompt(transcript: &str, context: Option<&str>) -> String {
    match context.map(str::trim).filter(|value| !value.is_empty()) {
        Some(context) => format!(
            "{SYSTEM_PROMPT}\n\nUse the selected context only for terminology, style, and local reference. Rewrite only the dictated text. Do not include the selected context unless the dictated text explicitly asks for it.\n\n{}\n\n{}",
            tagged("context", context),
            tagged("dictation", transcript),
        ),
        None => format!("{SYSTEM_PROMPT}\n\n{}", tagged("dictation", transcript)),
    }
}

/// Wrap `body` in XML-style tags to delimit the target text from the prompt and focus
/// the model's attention on what to operate on.
fn tagged(tag: &str, body: &str) -> String {
    format!("<{tag}>\n{body}\n</{tag}>")
}
