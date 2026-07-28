use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const LEGACY_BLOCK_SCHEMA_VERSION: u32 = 1;
pub const BLOCK_SCHEMA_VERSION: u32 = 2;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unattended: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning: Option<LearningRecord>,
}

impl ActivitySubject {
    pub fn is_learning(&self) -> bool {
        self.namespaces
            .first()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("learning"))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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

    pub fn with_learning_subjects(&self, learning_subjects: Vec<ActivitySubject>) -> Self {
        let mut subjects = self
            .subjects
            .iter()
            .filter(|subject| !subject.is_learning())
            .cloned()
            .collect::<Vec<_>>();
        subjects.extend(learning_subjects);
        Self {
            title: self.title.clone(),
            report: self.report.clone(),
            subjects,
        }
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
            if subject.learning.is_some() {
                return Err("base subjects must not contain learning metadata".to_string());
            }
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
            unattended: Some(subject.unattended),
            learning: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LearningNarrative {
    pub learning_subjects: Vec<ActivitySubject>,
}

impl LearningNarrative {
    pub fn parse(text: &str) -> Result<Self, String> {
        let generated: GeneratedLearningNarrative = serde_json::from_str(json_payload(text))
            .map_err(|error| {
                format!("learning response was not the required JSON object: {error}")
            })?;
        if generated.learning_subjects.is_empty() {
            return Err("learning_subjects is empty".to_string());
        }
        let learning_subjects = generated
            .learning_subjects
            .into_iter()
            .map(ActivitySubject::from)
            .collect::<Vec<_>>();
        for subject in &learning_subjects {
            validate_common_subject(subject)?;
            if !subject.is_learning() {
                return Err("learning subject is not rooted at learning".to_string());
            }
            if subject.learning.is_none() {
                return Err("learning subject is missing learning metadata".to_string());
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
    unattended: bool,
    learning: LearningRecord,
}

impl From<GeneratedLearningSubject> for ActivitySubject {
    fn from(subject: GeneratedLearningSubject) -> Self {
        Self {
            namespaces: subject.namespaces,
            subject: subject.subject,
            estimated_duration_s: subject.estimated_duration_s,
            unattended: Some(subject.unattended),
            learning: Some(subject.learning),
        }
    }
}

fn validate_common_subject(subject: &ActivitySubject) -> Result<(), String> {
    if !(1..=4).contains(&subject.namespaces.len()) {
        return Err("subject namespaces must contain between 1 and 4 entries".to_string());
    }
    if subject.subject.trim().is_empty() {
        return Err("subject statement is empty".to_string());
    }
    if subject.estimated_duration_s == 0 {
        return Err("subject estimated_duration_s must be positive".to_string());
    }
    if subject.unattended.is_none() {
        return Err("subject unattended judgment is missing".to_string());
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
    InvalidModelOutput,
    InsufficientEvidence,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LearningEnrichment {
    pub status: LearningEnrichmentStatus,
    pub frames_sent: usize,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames_sent: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_frames_sent: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning_frames_sent: Option<usize>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning_enrichment: Option<LearningEnrichment>,
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
    pub learning_enrichment: Option<LearningEnrichment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BlockArtifact {
    pub fn is_supported(&self) -> bool {
        matches!(
            self.schema_version,
            LEGACY_BLOCK_SCHEMA_VERSION | BLOCK_SCHEMA_VERSION
        )
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
            learning_enrichment: self.learning_enrichment.clone(),
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
                "unattended": false,
                "learning": {
                    "search_queries": ["serde deny_unknown_fields"],
                    "sources": [{"kind":"documentation","title":"Container attributes"}],
                    "depth": "focused-explanation"
                }
            }]
        }"#;
        let narrative = LearningNarrative::parse(text).unwrap();
        let learning = narrative.learning_subjects[0].learning.as_ref().unwrap();
        assert_eq!(learning.depth, LearningDepth::FocusedExplanation);
        assert_eq!(learning.sources[0].kind, LearningSourceKind::Documentation);
        assert!(LearningNarrative::parse(&text.replace("learning\",", "education\",")).is_err());
        assert!(LearningNarrative::parse(&text.replace("documentation", "webpage")).is_err());
        assert!(LearningNarrative::parse(&text.replace("focused-explanation", "deep")).is_err());
    }

    #[test]
    fn learning_routing_normalizes_without_rewriting_the_namespace() {
        let subject = super::ActivitySubject {
            namespaces: vec![" Learning ".to_string(), "lookup".to_string()],
            subject: "Looked up a term.".to_string(),
            estimated_duration_s: 60,
            unattended: Some(false),
            learning: None,
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
    fn learning_merge_preserves_base_document_and_non_learning_subjects() {
        let base = ActivityNarrative::parse(
            r#"{"title":"Mixed block","report":"Worked and learned.","subjects":[
                {"namespaces":["software-development","coding","ashe-worker"],"subject":"Changed code.","estimated_duration_s":300,"unattended":false},
                {"namespaces":["Learning","lookup","rust"],"subject":"Read about Rust.","estimated_duration_s":120,"unattended":false}
            ]}"#,
        )
        .unwrap();
        let enriched = LearningNarrative::parse(
            r#"{"learning_subjects":[
                {"namespaces":["learning","lookup","rust","ownership"],"subject":"Looked up ownership.","estimated_duration_s":60,"unattended":false,"learning":{"search_queries":[],"sources":[],"depth":"lookup"}},
                {"namespaces":["learning","applied","rust","borrowing"],"subject":"Applied a borrowing rule.","estimated_duration_s":60,"unattended":false,"learning":{"search_queries":[],"sources":[{"kind":"code"}],"depth":"applied"}}
            ]}"#,
        )
        .unwrap();
        let merged = base.with_learning_subjects(enriched.learning_subjects);
        assert_eq!(merged.title, "Mixed block");
        assert_eq!(merged.report, "Worked and learned.");
        assert_eq!(merged.subjects.len(), 3);
        assert_eq!(merged.subjects[0].namespaces[0], "software-development");
    }

    #[test]
    fn stored_version_one_and_two_artifacts_are_supported() {
        let version_one = r#"{
            "schema_version":1,"block":"2026-07-28T1200","window_start":1,"window_end":2,
            "outcome":"described","active_seconds":1,"idle_seconds":0,"app_seconds":{},
            "timeline":[],"frames_captured":1,"frames_sent":1,"subjects":[
                {"namespaces":["work"],"subject":"Worked.","estimated_duration_s":1}
            ]
        }"#;
        let old: BlockArtifact = serde_json::from_str(version_one).unwrap();
        assert!(old.is_supported());
        assert_eq!(old.frames_sent, Some(1));
        assert_eq!(old.subjects[0].unattended, None);

        let version_two = version_one
            .replace(
                "\"schema_version\":1",
                &format!("\"schema_version\":{BLOCK_SCHEMA_VERSION}"),
            )
            .replace("\"frames_sent\":1,", "\"base_frames_sent\":1,")
            .replace(
                "\"estimated_duration_s\":1}",
                "\"estimated_duration_s\":1,\"unattended\":false}",
            );
        let new: BlockArtifact = serde_json::from_str(&version_two).unwrap();
        assert!(new.is_supported());
        assert_eq!(new.base_frames_sent, Some(1));
        assert_eq!(new.subjects[0].unattended, Some(false));
    }

    #[test]
    fn structured_outputs_accept_one_exact_outer_fence_only() {
        let object = r#"{"title":"Focused work","report":"A concise report.","subjects":[{"namespaces":["software-development"],"subject":"Worked.","estimated_duration_s":60,"unattended":false}]}"#;
        assert!(ActivityNarrative::parse(&format!("```json\n{object}\n```")).is_ok());
        assert!(ActivityNarrative::parse(&format!("preamble\n{object}")).is_err());
    }
}
