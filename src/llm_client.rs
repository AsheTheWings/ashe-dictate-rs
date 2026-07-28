use crate::block_artifact::{ActivityNarrative, LearningNarrative};
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

const ACTIVITY_PROMPT: &str = "You are Ashe Worker's activity journal keeper. Produce a factual account of what the user did during this time block, whether software development, learning, entertainment, social media, messaging, shopping, games, or personal administration. Treat the measured app/window timeline as ground truth. Read visible details closely, describe progression rather than listing images, never score productivity, and do not mention screenshots, frames, telemetry, or yourself. Keep credentials, financial values, medical details, and intimate conversations at a safe high level. Emit a learning-rooted subject whenever the user meaningfully acquires, examines, follows, applies, or synthesizes information, including a term lookup or informational media consumption. Learning may overlap another supported activity subject. Informational or intellectual media is learning; narrative leisure media and sports are entertainment. This base request identifies learning provisionally but does not produce learning-specific metadata.";
const LEARNING_PROMPT: &str = "You are Ashe Worker's learning-block analyst. From the measured context, provisional base narrative, and chronological visual evidence, return the evidence-backed atomic learning units covered in this block. Each unit must describe exactly one term, concept, mechanism, procedure, comparison, or application. Report only what was visibly searched, examined, followed, compared, or applied. Do not claim mastery, retention, or understanding. Do not mention screenshots, frames, telemetry, prompts, JSON, or yourself. Keep credentials, financial values, medical details, and intimate conversations at a safe high level.";
const SUMMARY_PROMPT: &str = "Maintain a continuous rolling summary of the user's computer activity from the structured JSON input. Merge previous_summary with blocks. Preserve concrete project, app, document, site, game, media, person, and open-task names; compress routine detail; preserve rough chronology and current state. Preserve material atomic learning topics, important visible source identity, and evidence-backed learning depth without claiming comprehension or retention. The blocks array is the only new material. Treat measured fields as ground truth and do not infer later activity. Return Markdown prose only, without a title or preamble.";

pub async fn describe_activity_block(
    config: AppConfig,
    context: String,
    frames: Vec<(String, Vec<u8>)>,
    duration_budget_s: u64,
) -> Result<ActivityNarrative> {
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
    let instructions = format!(
        "{ACTIVITY_PROMPT}\n\nReturn exactly one JSON object and nothing else: no Markdown fence, preamble, or trailing commentary. The object must contain exactly title, report, and subjects. title is a short plain-text string in the form 'Area: what happened'. report is 120-250 words of concise Markdown prose. subjects is an array of 1-8 sustained, meaningfully distinct activity subjects; do not split incidental actions into separate entries. Each subject object must contain exactly namespaces, subject, estimated_duration_s, and unattended. namespaces is an ordered array of 1-4 unique lowercase kebab-case strings. Level 1 is the broad domain; level 2 is the domain-specific activity mode or channel; level 3 is the domain-specific grouping such as project, media type, knowledge field, or social surface; level 4 is the concrete focus such as project topic, media title, event, thread topic, or atomic learning topic. Prefer these exact roots when applicable: software-development, entertainment, social-media, learning. Under software-development use agentic-coding when an LLM coding agent primarily performs the development and coding when the user primarily codes manually. Under entertainment use streaming followed by movie, tv-show, live-stream, or football-match when supported. Under social-media use the canonical service slug such as x-com. Under learning use a supported mode such as lookup, reading, watching, coursework, practice, or discussion. Do not invent specificity. Put actions and outcomes in subject, not namespaces, and use consistent namespace spelling within the response. subject is one self-contained factual statement. estimated_duration_s is a positive integer estimating time spent on that subject and must remain within the {duration_budget_s} seconds of measured coverage. unattended is true only when the subject appears to have continued for most of its estimated duration with little evidence of user interaction or attention. Measured input idle is evidence, not proof of absence. Default passive reading, films, television, and live streams to unattended=false unless the combined timeline and progression reasonably show that they were left running or left unchanged. Judge each subject independently: subjects may overlap when the user multitasks, so their estimated durations do not need to sum to the coverage duration. Uncertain, idle, or unclassified time may remain unallocated; passive activity may receive time even without keyboard or mouse input. Use the measured timeline and visible progression rather than dividing time evenly."
    );
    let mut body = json!({
        "model": config.tera_model,
        "instructions": instructions,
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
    let narrative = ActivityNarrative::parse(&extract_output_text(&payload))
        .map_err(|message| anyhow::Error::new(InvalidActivityOutput(message)))?;
    narrative
        .validate_duration_budget(duration_budget_s)
        .map_err(|message| anyhow::Error::new(InvalidActivityOutput(message)))?;
    Ok(narrative)
}

pub async fn describe_learning_block(
    config: AppConfig,
    context: String,
    frames: Vec<(String, Vec<u8>)>,
    duration_budget_s: u64,
) -> Result<LearningNarrative> {
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
    let instructions = format!(
        "{LEARNING_PROMPT}\n\nReturn exactly one JSON object with exactly one field, learning_subjects, and nothing else. learning_subjects is a non-empty array of atomic learning subjects. Each subject contains exactly namespaces, subject, estimated_duration_s, unattended, and learning. namespaces contains 1-4 ordered lowercase kebab-case strings rooted at learning; use level 2 for the learning mode, level 3 for the knowledge field, and level 4 for the atomic topic when supported. subject is one cautious factual statement using observable verbs such as looked up, reviewed, worked through, compared, or applied. estimated_duration_s is positive and must remain within {duration_budget_s} seconds of measured coverage; independently judged subjects may overlap. unattended follows the base subject rule and must not equate passive consumption with absence. learning contains exactly search_queries, sources, and depth. search_queries contains only visibly entered or displayed queries. sources contains visible source objects: each has kind and may have title, provider, and creator only when supported. kind is exactly one of search-results, documentation, article, paper, video, course, book, forum, social-post, code, or other. depth is the strongest observable treatment: lookup for a query, snippet, definition, or short answer; orientation for purpose and high-level structure; focused-explanation for mechanism, relationships, rationale, examples, or tradeoffs; procedural for followed steps; applied for visible use in an active task; synthesis for visibly comparing or combining multiple sources or ideas. Choose the lower supported depth when uncertain. Time spent, source prestige, technical difficulty, multiple open tabs, and input-idle state do not by themselves establish depth."
    );
    let mut body = json!({
        "model": config.tera_model,
        "instructions": instructions,
        "input": [{ "role": "user", "content": content }]
    });
    apply_reasoning_effort(&mut body, config.llm_reasoning_effort.as_deref());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("failed to build learning HTTP client")?;
    let response = client
        .post(endpoint)
        .bearer_auth(&config.tera_api_key)
        .json(&body)
        .send()
        .await
        .context("learning description request failed")?;
    let status = response.status();
    let payload: Value = response
        .json()
        .await
        .context("learning description returned a non-JSON body")?;
    if !status.is_success() {
        let message = payload
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| payload.get("detail").and_then(Value::as_str))
            .unwrap_or("unknown error");
        return Err(anyhow!("learning description failed ({status}): {message}"));
    }
    let narrative = LearningNarrative::parse(&extract_output_text(&payload))
        .map_err(|message| anyhow::Error::new(InvalidLearningOutput(message)))?;
    narrative
        .validate_duration_budget(duration_budget_s)
        .map_err(|message| anyhow::Error::new(InvalidLearningOutput(message)))?;
    Ok(narrative)
}

pub async fn refresh_activity_summary(
    config: AppConfig,
    source: Value,
    max_chars: usize,
) -> Result<String> {
    let endpoint = format!("{}/responses", config.tera_api_base.trim_end_matches('/'));
    let input = serde_json::to_string_pretty(&source)
        .context("failed to serialize rolling-summary source")?;
    let mut body = json!({
        "model": config.tera_model,
        "instructions": format!("{SUMMARY_PROMPT}\n\nThe complete result must contain at most {max_chars} Unicode characters."),
        "input": [{
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": input,
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

#[derive(Debug)]
pub struct InvalidActivityOutput(pub String);

impl std::fmt::Display for InvalidActivityOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid structured activity output: {}", self.0)
    }
}

impl std::error::Error for InvalidActivityOutput {}

#[derive(Debug)]
pub struct InvalidLearningOutput(pub String);

impl std::fmt::Display for InvalidLearningOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid structured learning output: {}", self.0)
    }
}

impl std::error::Error for InvalidLearningOutput {}

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
