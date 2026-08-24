//! Model-response parsing: tolerate code fences and surrounding prose, then
//! demand valid JSON between the first `{` and the last `}`. No auto-retry —
//! the error carries a sample and the caller decides.

use serde::Deserialize;

use crate::EngineError;

#[derive(Debug, Deserialize)]
pub struct RawGroups {
    #[serde(default)]
    pub groups: Vec<RawGroup>,
}

#[derive(Debug, Deserialize)]
pub struct RawGroup {
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub classes: Vec<String>,
    /// Unknown values fall back to "focus" downstream (when in doubt, focus).
    #[serde(default = "default_effort")]
    pub effort: String,
    #[serde(default)]
    pub reason: String,
}

fn default_effort() -> String {
    "focus".to_string()
}

pub fn parse_response(text: &str) -> Result<RawGroups, EngineError> {
    let start = text.find('{');
    let end = text.rfind('}');
    let (Some(start), Some(end)) = (start, end) else {
        return Err(err("no JSON object in response", text));
    };
    if end < start {
        return Err(err("no JSON object in response", text));
    }
    serde_json::from_str(&text[start..=end]).map_err(|e| err(&e.to_string(), text))
}

fn err(msg: &str, text: &str) -> EngineError {
    let sample: String = text.chars().take(300).collect();
    EngineError::GroupingParse {
        msg: msg.to_string(),
        sample,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_json_parses() {
        let r = parse_response(r#"{"groups": [{"label": "x", "classes": ["C0"]}]}"#).unwrap();
        assert_eq!(r.groups.len(), 1);
        assert_eq!(r.groups[0].effort, "focus");
    }

    #[test]
    fn fenced_json_parses() {
        let r = parse_response(
            "```json\n{\"groups\": [{\"label\": \"x\", \"classes\": [\"C0\"], \"effort\": \"skim\"}]}\n```",
        )
        .unwrap();
        assert_eq!(r.groups[0].effort, "skim");
    }

    #[test]
    fn prose_wrapped_json_parses() {
        let r = parse_response(
            "Here is the grouping you asked for:\n{\"groups\": []}\nHope that helps!",
        )
        .unwrap();
        assert!(r.groups.is_empty());
    }

    #[test]
    fn garbage_errors_with_sample() {
        match parse_response("I cannot help with that.") {
            Err(EngineError::GroupingParse { sample, .. }) => {
                assert!(sample.contains("cannot help"));
            }
            other => panic!("expected GroupingParse, got {other:?}"),
        }
    }
}
