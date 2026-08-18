use std::collections::BTreeMap;

use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TEMPLATE_SCHEMA_VERSION: u32 = 1;
const MAX_NESTING_DEPTH: usize = 16;
const MAX_STEPS: usize = 1_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TemplateDefinition {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub variables: BTreeMap<String, Value>,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    Request(RequestStep),
    Extract(ExtractStep),
    If {
        condition: String,
        then: Vec<Step>,
        #[serde(rename = "else", default)]
        otherwise: Vec<Step>,
    },
    ForEach {
        item: String,
        items: String,
        steps: Vec<Step>,
    },
    Delay {
        milliseconds: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RequestStep {
    pub name: String,
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    pub body: Option<RequestBody>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum RequestBody {
    Json(Value),
    Text(String),
    Form(BTreeMap<String, String>),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExtractStep {
    pub name: String,
    pub source: ExtractSource,
    pub selector: String,
    pub target: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractSource {
    Json,
    Text,
    Header,
    Status,
}

impl TemplateDefinition {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == TEMPLATE_SCHEMA_VERSION,
            "unsupported template version"
        );
        ensure!(
            !self.name.trim().is_empty(),
            "template name cannot be empty"
        );
        ensure!(
            !self.steps.is_empty(),
            "template must contain at least one step"
        );
        let mut count = 0;
        validate_steps(&self.steps, 0, &mut count)
    }
}

fn validate_steps(steps: &[Step], depth: usize, count: &mut usize) -> Result<()> {
    ensure!(depth <= MAX_NESTING_DEPTH, "template nesting is too deep");
    *count += steps.len();
    ensure!(*count <= MAX_STEPS, "template has too many steps");
    for step in steps {
        match step {
            Step::Request(request) => {
                ensure!(
                    !request.name.trim().is_empty(),
                    "request name cannot be empty"
                );
                ensure!(
                    !request.url.trim().is_empty(),
                    "request URL cannot be empty"
                );
                let method = request.method.to_ascii_uppercase();
                ensure!(
                    matches!(
                        method.as_str(),
                        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
                    ),
                    "unsupported HTTP method: {}",
                    request.method
                );
            }
            Step::Extract(extract) => {
                ensure!(
                    !extract.name.trim().is_empty(),
                    "extract name cannot be empty"
                );
                ensure!(
                    !extract.target.trim().is_empty(),
                    "extract target cannot be empty"
                );
                if !matches!(extract.source, ExtractSource::Status) && extract.selector.is_empty() {
                    return Err(anyhow!("extract selector cannot be empty"));
                }
            }
            Step::If {
                condition,
                then,
                otherwise,
            } => {
                ensure!(!condition.trim().is_empty(), "if condition cannot be empty");
                validate_steps(then, depth + 1, count)?;
                validate_steps(otherwise, depth + 1, count)?;
            }
            Step::ForEach { item, items, steps } => {
                ensure!(!item.trim().is_empty(), "for_each item cannot be empty");
                ensure!(!items.trim().is_empty(), "for_each items cannot be empty");
                validate_steps(steps, depth + 1, count)?;
            }
            Step::Delay { milliseconds } => {
                ensure!(*milliseconds <= 300_000, "delay exceeds five minutes");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates_v1_template() {
        let template: TemplateDefinition = serde_json::from_value(serde_json::json!({
            "version": 1,
            "name": "health check",
            "variables": {"base_url": "https://example.com"},
            "steps": [{
                "type": "request",
                "name": "fetch",
                "method": "GET",
                "url": "{{base_url}}/health"
            }, {
                "type": "extract",
                "name": "capture status",
                "source": "status",
                "selector": "",
                "target": "status"
            }]
        }))
        .unwrap();
        template.validate().unwrap();
    }

    #[test]
    fn rejects_unknown_method() {
        let template = TemplateDefinition {
            version: 1,
            name: "invalid".into(),
            variables: BTreeMap::new(),
            steps: vec![Step::Request(RequestStep {
                name: "bad".into(),
                method: "TRACE".into(),
                url: "https://example.com".into(),
                headers: BTreeMap::new(),
                query: BTreeMap::new(),
                body: None,
            })],
        };
        assert!(template.validate().is_err());
    }
}
