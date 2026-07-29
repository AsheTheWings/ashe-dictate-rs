use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const BLOCK_SCHEMA_VERSION: u32 = 2;
pub const LEARNING_ARTIFACT_SCHEMA_VERSION: u32 = 3;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LearningSourceKind {
    SearchResults,
    Documentation,
    Article,
    Paper,
    Video,
    Course,
    Book,
    Forum,
    SocialPost,
    Code,
    Other,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LearningDepth {
    Lookup,
    Orientation,
    FocusedExplanation,
    Procedural,
    Applied,
    Synthesis,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LearningSource {
    pub kind: LearningSourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LearningRecord {
    pub search_queries: Vec<String>,
    pub sources: Vec<LearningSource>,
    pub depth: LearningDepth,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivitySubject {
    pub namespaces: Vec<String>,
    pub subject: String,
    pub estimated_duration_s: u64,
    pub unattended: bool,
}

impl ActivitySubject {
    pub fn is_learning(&self) -> bool {
        self.namespaces
            .first()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("learning"))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityNarrative {
    pub title: String,
    pub report: String,
    pub subjects: Vec<ActivitySubject>,
}

impl ActivityNarrative {
    pub fn parse(text: &str) -> Result<Self, String> {
        let generated: GeneratedActivityNarrative = serde_json::from_str(json_payload(text))
            .map_err(|error| format!("response was not the required JSON object: {error}"))?;
        let narrative = Self {
            title: generated.title,
            report: generated.report,
            subjects: generated
                .subjects
                .into_iter()
                .map(ActivitySubject::from)
                .collect(),
        };
        narrative.validate()?;
        Ok(narrative)
    }

    pub fn has_learning(&self) -> bool {
        self.subjects.iter().any(ActivitySubject::is_learning)
    }

    pub fn validate_duration_budget(&self, duration_budget_s: u64) -> Result<(), String> {
        if let Some(subject) = self
            .subjects
            .iter()
            .find(|subject| subject.estimated_duration_s > duration_budget_s)
        {
            return Err(format!(
                "subject estimated_duration_s {} exceeds measured coverage {duration_budget_s}",
                subject.estimated_duration_s
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() {
            return Err("title is empty".to_string());
        }
        if self.report.trim().is_empty() {
            return Err("report is empty".to_string());
        }
        if !(1..=8).contains(&self.subjects.len()) {
            return Err("subjects must contain between 1 and 8 entries".to_string());
        }
        for subject in &self.subjects {
            validate_common_subject(subject)?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratedActivityNarrative {
    title: String,
    report: String,
    subjects: Vec<GeneratedActivitySubject>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratedActivitySubject {
    namespaces: Vec<String>,
    subject: String,
    estimated_duration_s: u64,
    unattended: bool,
}

impl From<GeneratedActivitySubject> for ActivitySubject {
    fn from(subject: GeneratedActivitySubject) -> Self {
        Self {
            namespaces: subject.namespaces,
            subject: subject.subject,
            estimated_duration_s: subject.estimated_duration_s,
            unattended: subject.unattended,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LearningNarrative {
    pub learning_subjects: Vec<LearningSubject>,
}

impl LearningNarrative {
    pub fn parse(text: &str) -> Result<Self, String> {
        let generated: GeneratedLearningNarrative = serde_json::from_str(json_payload(text))
            .map_err(|error| {
                format!("learning response was not the required JSON object: {error}")
            })?;
        let learning_subjects = generated
            .learning_subjects
            .into_iter()
            .map(LearningSubject::from)
            .collect::<Vec<_>>();
        for subject in &learning_subjects {
            validate_subject_fields(
                &subject.namespaces,
                &subject.subject,
                subject.estimated_duration_s,
            )?;
            if !subject.is_learning() {
                return Err("learning subject is not rooted at learning".to_string());
            }
        }
        Ok(Self { learning_subjects })
    }

    pub fn validate_duration_budget(&self, duration_budget_s: u64) -> Result<(), String> {
        if let Some(subject) = self
            .learning_subjects
            .iter()
            .find(|subject| subject.estimated_duration_s > duration_budget_s)
        {
            return Err(format!(
                "learning subject estimated_duration_s {} exceeds measured coverage {duration_budget_s}",
                subject.estimated_duration_s
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratedLearningNarrative {
    learning_subjects: Vec<GeneratedLearningSubject>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratedLearningSubject {
    namespaces: Vec<String>,
    subject: String,
    estimated_duration_s: u64,
    learning: LearningRecord,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LearningSubject {
    pub namespaces: Vec<String>,
    pub subject: String,
    pub estimated_duration_s: u64,
    pub learning: LearningRecord,
}

impl LearningSubject {
    pub fn is_learning(&self) -> bool {
        self.namespaces
            .first()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("learning"))
    }
}

impl From<GeneratedLearningSubject> for LearningSubject {
    fn from(subject: GeneratedLearningSubject) -> Self {
        Self {
            namespaces: subject.namespaces,
            subject: subject.subject,
            estimated_duration_s: subject.estimated_duration_s,
            learning: subject.learning,
        }
    }
}

fn validate_common_subject(subject: &ActivitySubject) -> Result<(), String> {
    validate_subject_fields(
        &subject.namespaces,
        &subject.subject,
        subject.estimated_duration_s,
    )
}

fn validate_subject_fields(
    namespaces: &[String],
    subject: &str,
    estimated_duration_s: u64,
) -> Result<(), String> {
    if !(1..=4).contains(&namespaces.len()) {
        return Err("subject namespaces must contain between 1 and 4 entries".to_string());
    }
    if subject.trim().is_empty() {
        return Err("subject statement is empty".to_string());
    }
    if estimated_duration_s == 0 {
        return Err("subject estimated_duration_s must be positive".to_string());
    }
    Ok(())
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LearningEnrichmentStatus {
    Complete,
    NoLearning,
    InvalidModelOutput,
    InsufficientEvidence,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LearningArtifact {
    pub schema_version: u32,
    pub block: String,
    pub window_start: i64,
    pub window_end: i64,
    pub status: LearningEnrichmentStatus,
    pub frames_sent: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub subjects: Vec<LearningSubject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    pub base_frames_sent: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyframe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
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
    pub fn has_current_schema(&self) -> bool {
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
    use super::{
        ActivityNarrative, BLOCK_SCHEMA_VERSION, BlockArtifact, LearningDepth, LearningNarrative,
        LearningSourceKind,
    };

    #[test]
    fn base_narrative_requires_unattended_and_rejects_learning_metadata() {
        let valid = r#"{
            "title": "Focused work",
            "report": "A concise report.",
            "subjects": [{
                "namespaces": ["software-development", "coding", "ashe-worker"],
                "subject": "Implemented the planned change.",
                "estimated_duration_s": 420,
                "unattended": false
            }]
        }"#;
        assert!(ActivityNarrative::parse(valid).is_ok());
        assert!(
            ActivityNarrative::parse(
                &valid.replace(",\n                \"unattended\": false", "")
            )
            .is_err()
        );
        assert!(
            ActivityNarrative::parse(&valid.replace(
                "\"unattended\": false",
                "\"unattended\": false, \"learning\": null"
            ))
            .is_err()
        );
    }

    #[test]
    fn learning_narrative_enforces_root_source_kind_and_depth() {
        let text = r#"{
            "learning_subjects": [{
                "namespaces": ["learning", "lookup", "rust", "serde-attributes"],
                "subject": "Looked up how a Serde container attribute behaves.",
                "estimated_duration_s": 180,
                "learning": {
                    "search_queries": ["serde deny_unknown_fields"],
                    "sources": [{"kind":"documentation","title":"Container attributes"}],
                    "depth": "focused-explanation"
                }
            }]
        }"#;
        let narrative = LearningNarrative::parse(text).unwrap();
        let learning = &narrative.learning_subjects[0].learning;
        assert_eq!(learning.depth, LearningDepth::FocusedExplanation);
        assert_eq!(learning.sources[0].kind, LearningSourceKind::Documentation);
        assert!(LearningNarrative::parse(&text.replace("learning\",", "education\",")).is_err());
        assert!(LearningNarrative::parse(&text.replace("documentation", "webpage")).is_err());
        assert!(LearningNarrative::parse(&text.replace("focused-explanation", "deep")).is_err());
        assert!(
            LearningNarrative::parse(
                &text.replace("\"learning\": {", "\"unattended\": false, \"learning\": {")
            )
            .is_err()
        );
    }

    #[test]
    fn learning_narrative_accepts_an_empty_validated_result() {
        let narrative = LearningNarrative::parse(r#"{"learning_subjects":[]}"#).unwrap();
        assert!(narrative.learning_subjects.is_empty());
    }

    #[test]
    fn no_learning_status_is_explicit_in_the_artifact_schema() {
        let artifact = super::LearningArtifact {
            schema_version: super::LEARNING_ARTIFACT_SCHEMA_VERSION,
            block: "2026-07-29T1200".to_string(),
            window_start: 1,
            window_end: 2,
            status: super::LearningEnrichmentStatus::NoLearning,
            frames_sent: 3,
            model: Some("model".to_string()),
            subjects: Vec::new(),
            error: None,
        };
        let value = serde_json::to_value(artifact).unwrap();
        assert_eq!(value["schema_version"], 3);
        assert_eq!(value["status"], "no-learning");
        assert_eq!(value["subjects"], serde_json::json!([]));
    }

    #[test]
    fn learning_routing_normalizes_without_rewriting_the_namespace() {
        let subject = super::ActivitySubject {
            namespaces: vec![" Learning ".to_string(), "lookup".to_string()],
            subject: "Looked up a term.".to_string(),
            estimated_duration_s: 60,
            unattended: false,
        };
        assert!(subject.is_learning());
        assert_eq!(subject.namespaces[0], " Learning ");

        let non_learning = super::ActivitySubject {
            namespaces: vec!["software-development".to_string()],
            ..subject
        };
        assert!(!non_learning.is_learning());
    }

    #[test]
    fn learning_artifact_is_separate_from_the_base_narrative() {
        let base = ActivityNarrative::parse(
            r#"{"title":"Mixed block","report":"Worked and learned.","subjects":[
                {"namespaces":["software-development","coding","ashe-worker"],"subject":"Changed code.","estimated_duration_s":300,"unattended":false},
                {"namespaces":["Learning","lookup","rust"],"subject":"Read about Rust.","estimated_duration_s":120,"unattended":false}
            ]}"#,
        )
        .unwrap();
        let enriched = LearningNarrative::parse(
            r#"{"learning_subjects":[
                {"namespaces":["learning","lookup","rust","ownership"],"subject":"Looked up ownership.","estimated_duration_s":60,"learning":{"search_queries":[],"sources":[],"depth":"lookup"}},
                {"namespaces":["learning","applied","rust","borrowing"],"subject":"Applied a borrowing rule.","estimated_duration_s":60,"learning":{"search_queries":[],"sources":[{"kind":"code"}],"depth":"applied"}}
            ]}"#,
        )
        .unwrap();
        let artifact = super::LearningArtifact {
            schema_version: super::LEARNING_ARTIFACT_SCHEMA_VERSION,
            block: "2026-07-28T1200".to_string(),
            window_start: 1,
            window_end: 2,
            status: super::LearningEnrichmentStatus::Complete,
            frames_sent: 3,
            model: Some("model".to_string()),
            subjects: enriched.learning_subjects,
            error: None,
        };

        assert_eq!(base.title, "Mixed block");
        assert_eq!(base.report, "Worked and learned.");
        assert_eq!(base.subjects.len(), 2);
        assert_eq!(artifact.subjects.len(), 2);
        assert!(
            artifact
                .subjects
                .iter()
                .all(super::LearningSubject::is_learning)
        );
    }

    #[test]
    fn only_the_current_strict_block_schema_is_accepted() {
        let current = format!(
            r#"{{
                "schema_version":{BLOCK_SCHEMA_VERSION},"block":"2026-07-28T1200",
                "window_start":1,"window_end":2,"outcome":"described",
                "active_seconds":1,"idle_seconds":0,"app_seconds":{{}},"timeline":[],
                "frames_captured":1,"base_frames_sent":1,"subjects":[
                    {{"namespaces":["software-development"],"subject":"Worked.",
                    "estimated_duration_s":1,"unattended":false}}
                ]
            }}"#
        );
        let artifact: BlockArtifact = serde_json::from_str(&current).unwrap();
        assert!(artifact.has_current_schema());
        assert_eq!(artifact.base_frames_sent, 1);

        assert!(
            serde_json::from_str::<BlockArtifact>(&current.replace(
                &format!("\"schema_version\":{BLOCK_SCHEMA_VERSION}"),
                "\"schema_version\":1"
            ))
            .is_ok_and(|artifact| !artifact.has_current_schema())
        );
        assert!(
            serde_json::from_str::<BlockArtifact>(
                &current.replace("\"base_frames_sent\":1", "\"frames_sent\":1")
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<BlockArtifact>(&current.replace(",\"unattended\":false", ""))
                .is_err()
        );
    }

    #[test]
    fn structured_outputs_accept_one_exact_outer_fence_only() {
        let object = r#"{"title":"Focused work","report":"A concise report.","subjects":[{"namespaces":["software-development"],"subject":"Worked.","estimated_duration_s":60,"unattended":false}]}"#;
        assert!(ActivityNarrative::parse(&format!("```json\n{object}\n```")).is_ok());
        assert!(ActivityNarrative::parse(&format!("preamble\n{object}")).is_err());
    }
}
