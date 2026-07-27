use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const BLOCK_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ActivitySubject {
    pub namespaces: Vec<String>,
    pub subject: String,
    pub estimated_duration_s: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ActivityNarrative {
    pub title: String,
    pub report: String,
    pub subjects: Vec<ActivitySubject>,
}

impl ActivityNarrative {
    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(json_payload(text))
            .map_err(|error| format!("response was not the required JSON object: {error}"))
    }
}

fn json_payload(text: &str) -> &str {
    let trimmed = text.trim();
    strip_outer_fence(trimmed, "```json")
        .or_else(|| strip_outer_fence(trimmed, "```"))
        .unwrap_or(trimmed)
        .trim()
}

fn strip_outer_fence<'a>(text: &'a str, opening: &str) -> Option<&'a str> {
    let body = text.strip_prefix(opening)?;
    let body = body
        .strip_prefix("\r\n")
        .or_else(|| body.strip_prefix('\n'))?;
    body.strip_suffix("```")
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BlockArtifact {
    pub schema_version: u32,
    pub block: String,
    pub window_start: i64,
    pub window_end: i64,
    pub outcome: String,
    pub active_seconds: u64,
    pub idle_seconds: u64,
    pub app_seconds: BTreeMap<String, u64>,
    pub timeline: Vec<String>,
    pub frames_captured: usize,
    pub frames_sent: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyframe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(default)]
    pub subjects: Vec<ActivitySubject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BlockDocumentInput {
    pub block: String,
    pub window_start: i64,
    pub window_end: i64,
    pub outcome: String,
    pub active_seconds: u64,
    pub idle_seconds: u64,
    pub app_seconds: BTreeMap<String, u64>,
    pub timeline: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    pub subjects: Vec<ActivitySubject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BlockArtifact {
    pub fn is_supported(&self) -> bool {
        self.schema_version == BLOCK_SCHEMA_VERSION
    }

    pub fn document_input(&self) -> BlockDocumentInput {
        BlockDocumentInput {
            block: self.block.clone(),
            window_start: self.window_start,
            window_end: self.window_end,
            outcome: self.outcome.clone(),
            active_seconds: self.active_seconds,
            idle_seconds: self.idle_seconds,
            app_seconds: self.app_seconds.clone(),
            timeline: self.timeline.clone(),
            title: self.title.clone(),
            report: self.report.clone(),
            subjects: self.subjects.clone(),
            reason: self.reason.clone(),
            error: self.error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ActivityNarrative;

    #[test]
    fn structured_narrative_accepts_hierarchical_namespaces_and_bounded_time() {
        let text = r#"{
            "title": "Focused work",
            "report": "A concise report.",
            "subjects": [
                {
                    "namespaces": ["work", "software-development", "project-name"],
                    "subject": "Implemented the planned change.",
                    "estimated_duration_s": 420
                }
            ]
        }"#;
        let narrative = ActivityNarrative::parse(text).unwrap();
        assert_eq!(narrative.subjects[0].estimated_duration_s, 420);
    }

    #[test]
    fn structured_narrative_accepts_one_exact_outer_fence() {
        let object = r#"{
            "title": "Focused work",
            "report": "A concise report.",
            "subjects": [
                {"namespaces":["work"],"subject":"Worked.","estimated_duration_s":60}
            ]
        }"#;
        for text in [
            format!("```json\n{object}\n```"),
            format!("```\n{object}\n```"),
        ] {
            assert!(ActivityNarrative::parse(&text).is_ok());
        }
    }

    #[test]
    fn structured_narrative_rejects_surrounding_commentary() {
        let text = r#"Here is the result:
        {"title":"Focused work","report":"A concise report.","subjects":[{"namespaces":["work"],"subject":"Worked.","estimated_duration_s":60}]}"#;
        assert!(ActivityNarrative::parse(text).is_err());
    }

    #[test]
    fn structured_narrative_preserves_parseable_model_judgments() {
        let text = r#"{
            "title": "Two subjects",
            "report": "A concise report.",
            "subjects": [
                {"namespaces":["Work", "software development", ""],"subject":"First.","estimated_duration_s":700},
                {"namespaces":[],"subject":"","estimated_duration_s":0}
            ]
        }"#;
        let narrative = ActivityNarrative::parse(text).unwrap();
        assert_eq!(narrative.subjects[0].estimated_duration_s, 700);
        assert!(narrative.subjects[1].namespaces.is_empty());
    }
}
