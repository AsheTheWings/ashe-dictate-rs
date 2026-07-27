use crate::config::AppConfig;
use anyhow::{Context, Result, anyhow};
use base64::Engine;
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

const ACTIVITY_PROMPT: &str = "You are Ashe Worker's activity journal keeper. Write a factual account of what the user did during this time block, whether work, reading, entertainment, messaging, shopping, games, or personal admin. Treat the measured app/window timeline as ground truth. Read visible details closely, describe progression rather than listing images, never score productivity, and do not mention screenshots, frames, telemetry, or yourself. Keep credentials, financial values, medical details, and intimate conversations at a safe high level. Output a short plain-text title on the first line in the form 'Area: what happened', then a blank line, then 120-250 words of concise Markdown prose.";
const SUMMARY_PROMPT: &str = "Maintain a continuous rolling summary of the user's computer activity. Merge the previous summary with the newly aged block reports. Preserve concrete project, app, document, site, game, media, person, and open-task names; compress routine detail; preserve rough chronology and current state. The supplied aged reports are the only new material. Do not infer later activity. Return Markdown prose only, without a title or preamble.";
const DAILY_REPORT_PROMPT: &str = "Write a human-readable end-of-day activity report from the complete block reports and measured coverage supplied by Ashe Worker. Use every relevant thread in proportion to its documented time, whether work, entertainment, browsing, communication, errands, or inactivity. Measured totals and coverage are ground truth: never invent activity inside missing intervals or infer durations from prose. Preserve concrete names and progression. Do not mention screenshots, telemetry, prompts, or being an observer. Do not score productivity or moralize. Protect credentials, financial values, medical details, and intimate conversation contents. Return Markdown with exactly these H2 sections: What happened, Loose ends, Time, and Coverage. Begin with a two-sentence overview before the first section. Omit no section; write 'No known loose ends' or 'Complete coverage' where appropriate. Do not add a top-level title.";

pub async fn describe_activity_block(
    config: AppConfig,
    context: String,
    frames: Vec<(String, Vec<u8>)>,
) -> Result<(String, String)> {
    let endpoint = format!("{}/responses", config.tera_api_base.trim_end_matches('/'));
    let mut content = vec![json!({ "type": "input_text", "text": context })];
    for (label, bytes) in frames {
        content.push(json!({ "type": "input_text", "text": label }));
        content.push(json!({
            "type": "input_image",
            "image_url": format!(
                "data:image/webp;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        }));
    }
    let mut body = json!({
        "model": config.tera_model,
        "instructions": ACTIVITY_PROMPT,
        "input": [{ "role": "user", "content": content }]
    });
    apply_reasoning_effort(&mut body, config.llm_reasoning_effort.as_deref());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("failed to build journal HTTP client")?;
    let response = client
        .post(endpoint)
        .bearer_auth(&config.tera_api_key)
        .json(&body)
        .send()
        .await
        .context("activity description request failed")?;
    let status = response.status();
    let payload: Value = response
        .json()
        .await
        .context("activity description returned a non-JSON body")?;
    if !status.is_success() {
        let message = payload
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| payload.get("detail").and_then(Value::as_str))
            .unwrap_or("unknown error");
        return Err(anyhow!("activity description failed ({status}): {message}"));
    }
    split_activity_report(&extract_output_text(&payload))
}

pub async fn refresh_activity_summary(
    config: AppConfig,
    previous: String,
    aged_reports: String,
    max_chars: usize,
) -> Result<String> {
    let endpoint = format!("{}/responses", config.tera_api_base.trim_end_matches('/'));
    let mut body = json!({
        "model": config.tera_model,
        "instructions": format!("{SUMMARY_PROMPT}\n\nThe complete result must contain at most {max_chars} Unicode characters."),
        "input": [{
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!(
                    "### Previous rolling summary\n{}\n\n### Block reports to fold in\n{}",
                    if previous.trim().is_empty() { "(none yet)" } else { previous.trim() },
                    aged_reports,
                )
            }]
        }]
    });
    apply_reasoning_effort(&mut body, config.llm_reasoning_effort.as_deref());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("failed to build summary HTTP client")?;
    let response = client
        .post(endpoint)
        .bearer_auth(&config.tera_api_key)
        .json(&body)
        .send()
        .await
        .context("rolling-summary request failed")?;
    let status = response.status();
    let payload: Value = response
        .json()
        .await
        .context("rolling-summary response was not JSON")?;
    if !status.is_success() {
        let message = payload
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| payload.get("detail").and_then(Value::as_str))
            .unwrap_or("unknown error");
        return Err(anyhow!(
            "rolling-summary request failed ({status}): {message}"
        ));
    }
    let summary = extract_output_text(&payload);
    let summary = summary.trim();
    if summary.is_empty() {
        return Err(anyhow!("rolling-summary response was empty"));
    }
    Ok(summary.chars().take(max_chars).collect())
}

pub async fn generate_daily_activity_report(
    config: AppConfig,
    day: String,
    timezone: String,
    measured_totals: String,
    coverage: String,
    complete_reports: String,
) -> Result<String> {
    let endpoint = format!("{}/responses", config.tera_api_base.trim_end_matches('/'));
    let mut body = json!({
        "model": config.tera_model,
        "instructions": DAILY_REPORT_PROMPT,
        "input": [{
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!(
                    "# Daily source for {day}\nTimezone: {timezone}\n\n## Measured daily totals\n{measured_totals}\n\n## Coverage and gaps\n{coverage}\n\n## Complete chronological block reports\n{complete_reports}\n\nWrite the report for {day} only.",
                )
            }]
        }]
    });
    apply_reasoning_effort(&mut body, config.llm_reasoning_effort.as_deref());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .context("failed to build daily-report HTTP client")?;
    let response = client
        .post(endpoint)
        .bearer_auth(&config.tera_api_key)
        .json(&body)
        .send()
        .await
        .context("daily-report request failed")?;
    let status = response.status();
    let payload: Value = response
        .json()
        .await
        .context("daily-report response was not JSON")?;
    if !status.is_success() {
        let message = payload
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| payload.get("detail").and_then(Value::as_str))
            .unwrap_or("unknown error");
        return Err(anyhow!("daily-report request failed ({status}): {message}"));
    }
    let report = extract_output_text(&payload);
    if report.trim().is_empty() {
        return Err(anyhow!("daily-report response was empty"));
    }
    Ok(report.trim().to_string())
}

fn split_activity_report(text: &str) -> Result<(String, String)> {
    let mut lines = text.trim().lines();
    let title = lines
        .find(|line| !line.trim().is_empty())
        .map(|line| {
            line.trim()
                .trim_start_matches('#')
                .trim()
                .trim_matches(['*', '"'])
        })
        .unwrap_or("Activity")
        .chars()
        .take(120)
        .collect::<String>();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    if title.is_empty() || body.is_empty() {
        return Err(anyhow!("activity description was empty or malformed"));
    }
    Ok((title, body))
}

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
/// The optional reasoning effort is forwarded when configured.
async fn request_response(
    config: &AppConfig,
    user_content: String,
    temperature: f32,
) -> Result<String> {
    let endpoint = format!("{}/responses", config.tera_api_base.trim_end_matches('/'));
    let mut body = json!({
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
    });
    apply_reasoning_effort(&mut body, config.llm_reasoning_effort.as_deref());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("failed to build reqwest client")?;
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

fn apply_reasoning_effort(body: &mut Value, effort: Option<&str>) {
    let Some(effort) = effort else {
        return;
    };
    if let Some(object) = body.as_object_mut() {
        object.insert("reasoning".to_string(), json!({ "effort": effort }));
    }
}

/// Collect the assistant-visible text from a Responses `output` array. Reasoning items
/// (which carry no `output_text`) are skipped; refusals are surfaced as their text.
fn extract_output_text(payload: &Value) -> String {
    let mut out = String::new();
    let Some(items) = payload.get("output").and_then(Value::as_array) else {
        return out;
    };
    for item in items {
        if item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message")
            != "message"
        {
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

#[cfg(test)]
mod tests {
    use super::apply_reasoning_effort;
    use serde_json::json;

    #[test]
    fn reasoning_is_omitted_by_default() {
        let mut body = json!({ "model": "example" });
        apply_reasoning_effort(&mut body, None);
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn arbitrary_reasoning_effort_is_forwarded_unchanged() {
        let mut body = json!({ "model": "example" });
        apply_reasoning_effort(&mut body, Some("upstream-specific-value"));
        assert_eq!(
            body.pointer("/reasoning/effort"),
            Some(&json!("upstream-specific-value"))
        );
    }
}
