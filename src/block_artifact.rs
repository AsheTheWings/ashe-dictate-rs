use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const BLOCK_SCHEMA_VERSION: u32 = 3;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgenticCodingMedium {
    CodeEditor,
    Tui,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgenticCodingRecord {
    pub medium: AgenticCodingMedium,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agentic_coding: Option<AgenticCodingRecord>,
}

impl ActivitySubject {
    pub fn is_agentic_coding(&self) -> bool {
        self.namespaces
            .first()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("software-development"))
            && self
                .namespaces
                .get(1)
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("agentic-coding"))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityNarrative {
    pub title: String,
    pub report: String,
    pub subjects: Vec<ActivitySubject>,
    pub learning_subjects: Vec<LearningSubject>,
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
            learning_subjects: generated
                .learning_subjects
                .into_iter()
                .map(LearningSubject::from)
                .collect(),
        };
        narrative.validate()?;
        Ok(narrative)
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
        for subject in &self.learning_subjects {
            validate_subject_fields(
                &subject.namespaces,
                &subject.subject,
                subject.estimated_duration_s,
            )?;
            if !subject.is_learning() {
                return Err("learning subject is not rooted at learning".to_string());
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
    learning_subjects: Vec<GeneratedLearningSubject>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratedActivitySubject {
    namespaces: Vec<String>,
    subject: String,
    estimated_duration_s: u64,
    unattended: bool,
    agentic_coding: Option<AgenticCodingRecord>,
}

impl From<GeneratedActivitySubject> for ActivitySubject {
    fn from(subject: GeneratedActivitySubject) -> Self {
        Self {
            namespaces: subject.namespaces,
            subject: subject.subject,
            estimated_duration_s: subject.estimated_duration_s,
            unattended: subject.unattended,
            agentic_coding: subject.agentic_coding,
        }
    }
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
    )?;
    if subject.is_agentic_coding() != subject.agentic_coding.is_some() {
        return Err(
            "agentic_coding must be present exactly on software-development/agentic-coding subjects"
                .to_string(),
        );
    }
    if let Some(agentic) = &subject.agentic_coding {
        if agentic.tool.trim().is_empty() {
            return Err("agentic_coding tool is empty".to_string());
        }
        if agentic
            .model_id
            .as_ref()
            .is_some_and(|model_id| model_id.trim().is_empty())
        {
            return Err("agentic_coding model_id is empty".to_string());
        }
    }
    Ok(())
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
    pub subjects: Vec<ActivitySubject>,
    pub learning_subjects: Vec<LearningSubject>,
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
    pub learning_subjects: Vec<LearningSubject>,
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
            learning_subjects: self.learning_subjects.clone(),
            reason: self.reason.clone(),
            error: self.error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActivityNarrative, AgenticCodingMedium, BLOCK_SCHEMA_VERSION, BlockArtifact, LearningDepth,
        LearningSourceKind,
    };

    const UNIFIED: &str = r#"{
        "title":"Agentic coding: unified generation",
        "report":"Used Codex to revise the worker and reviewed the schema implications.",
        "subjects":[{
            "namespaces":["software-development","agentic-coding","ashe-worker"],
            "subject":"Used Codex to unify activity and learning generation.",
            "estimated_duration_s":300,
            "unattended":false,
            "agentic_coding":{"medium":"tui","tool":"codex","model_id":"gpt-5.4"}
        }],
        "learning_subjects":[{
            "namespaces":["learning","applied","rust","serde-schema"],
            "subject":"Applied strict Serde fields to a unified artifact schema.",
            "estimated_duration_s":120,
            "learning":{
                "search_queries":[],
                "sources":[{"kind":"code","title":"block_artifact.rs"}],
                "depth":"applied"
            }
        }]
    }"#;

    #[test]
    fn unified_narrative_preserves_activity_learning_and_agentic_metadata() {
        let narrative = ActivityNarrative::parse(UNIFIED).unwrap();
        let agentic = narrative.subjects[0].agentic_coding.as_ref().unwrap();
        assert_eq!(agentic.medium, AgenticCodingMedium::Tui);
        assert_eq!(agentic.tool, "codex");
        assert_eq!(agentic.model_id.as_deref(), Some("gpt-5.4"));
        let learning = &narrative.learning_subjects[0].learning;
        assert_eq!(learning.depth, LearningDepth::Applied);
        assert_eq!(learning.sources[0].kind, LearningSourceKind::Code);
    }

    #[test]
    fn unified_narrative_accepts_a_validated_empty_learning_result() {
        let mut value: serde_json::Value = serde_json::from_str(UNIFIED).unwrap();
        value["learning_subjects"] = serde_json::json!([]);
        let narrative = ActivityNarrative::parse(&serde_json::to_string(&value).unwrap()).unwrap();
        assert!(narrative.learning_subjects.is_empty());
    }

    #[test]
    fn agentic_metadata_is_required_exactly_on_agentic_coding_subjects() {
        assert!(ActivityNarrative::parse(&UNIFIED.replace(
            ",\n            \"agentic_coding\":{\"medium\":\"tui\",\"tool\":\"codex\",\"model_id\":\"gpt-5.4\"}",
            "",
        ))
        .is_err());
        assert!(
            ActivityNarrative::parse(&UNIFIED.replace("\"agentic-coding\"", "\"coding\"",))
                .is_err()
        );
    }

    #[test]
    fn agentic_metadata_accepts_a_code_editor_without_a_visible_model() {
        let text = UNIFIED
            .replace("\"medium\":\"tui\"", "\"medium\":\"code-editor\"")
            .replace("\"tool\":\"codex\"", "\"tool\":\"cursor\"")
            .replace(",\"model_id\":\"gpt-5.4\"", "");
        let narrative = ActivityNarrative::parse(&text).unwrap();
        let agentic = narrative.subjects[0].agentic_coding.as_ref().unwrap();
        assert_eq!(agentic.medium, AgenticCodingMedium::CodeEditor);
        assert_eq!(agentic.tool, "cursor");
        assert_eq!(agentic.model_id, None);
    }

    #[test]
    fn unified_narrative_enforces_learning_root_source_kind_and_depth() {
        assert!(
            ActivityNarrative::parse(
                &UNIFIED.replace("\"learning\",\"applied\"", "\"education\",\"applied\"",)
            )
            .is_err()
        );
        assert!(ActivityNarrative::parse(&UNIFIED.replace("\"code\"", "\"webpage\"")).is_err());
        assert!(ActivityNarrative::parse(&UNIFIED.replace("\"applied\"", "\"deep\"")).is_err());
    }

    #[test]
    fn only_the_current_strict_block_schema_is_accepted() {
        let current = format!(
            r#"{{
                "schema_version":{BLOCK_SCHEMA_VERSION},"block":"2026-07-28T1200",
                "window_start":1,"window_end":2,"outcome":"described",
                "active_seconds":1,"idle_seconds":0,"app_seconds":{{}},"timeline":[],
                "frames_captured":1,"frames_sent":1,"subjects":[
                    {{"namespaces":["software-development"],"subject":"Worked.",
                    "estimated_duration_s":1,"unattended":false}}
                ],"learning_subjects":[]
            }}"#
        );
        let artifact: BlockArtifact = serde_json::from_str(&current).unwrap();
        assert!(artifact.has_current_schema());
        assert_eq!(artifact.frames_sent, 1);

        assert!(
            serde_json::from_str::<BlockArtifact>(&current.replace(
                &format!("\"schema_version\":{BLOCK_SCHEMA_VERSION}"),
                "\"schema_version\":1"
            ))
            .is_ok_and(|artifact| !artifact.has_current_schema())
        );
        assert!(
            serde_json::from_str::<BlockArtifact>(
                &current.replace("\"frames_sent\":1", "\"base_frames_sent\":1")
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
        let object = r#"{"title":"Focused work","report":"A concise report.","subjects":[{"namespaces":["software-development"],"subject":"Worked.","estimated_duration_s":60,"unattended":false}],"learning_subjects":[]}"#;
        assert!(ActivityNarrative::parse(&format!("```json\n{object}\n```")).is_ok());
        assert!(ActivityNarrative::parse(&format!("preamble\n{object}")).is_err());
    }
}
