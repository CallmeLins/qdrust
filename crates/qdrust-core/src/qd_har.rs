use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct QdHar {
    raw: Value,
    document: HarDocument,
}

impl QdHar {
    pub fn parse(raw: Value) -> Result<Self> {
        let document: HarDocument =
            serde_json::from_value(raw.clone()).context("invalid QD HAR document")?;
        ensure!(document.log.version == "1.2", "unsupported HAR version");
        ensure!(
            !document.log.entries.is_empty(),
            "QD HAR must contain at least one entry"
        );
        for (index, entry) in document
            .log
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.checked)
        {
            entry
                .validate()
                .with_context(|| format!("invalid QD HAR entry {}", index + 1))?;
        }
        Ok(Self { raw, document })
    }

    pub fn raw(&self) -> &Value {
        &self.raw
    }

    pub fn entries(&self) -> &[QdHarEntry] {
        &self.document.log.entries
    }

    pub fn enabled_entries(&self) -> impl Iterator<Item = &QdHarEntry> {
        self.entries().iter().filter(|entry| entry.checked)
    }
}

#[derive(Clone, Debug, Deserialize)]
struct HarDocument {
    log: HarLog,
}

#[derive(Clone, Debug, Deserialize)]
struct HarLog {
    version: String,
    entries: Vec<QdHarEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QdHarEntry {
    #[serde(default)]
    pub checked: bool,
    #[serde(default)]
    pub comment: Option<String>,
    pub request: QdHarRequest,
    #[serde(default)]
    pub success_asserts: Vec<QdRule>,
    #[serde(default)]
    pub failed_asserts: Vec<QdRule>,
    #[serde(default)]
    pub extract_variables: Vec<QdExtractRule>,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}

impl QdHarEntry {
    fn validate(&self) -> Result<()> {
        ensure!(!self.request.method.trim().is_empty(), "method is empty");
        ensure!(!self.request.url.trim().is_empty(), "URL is empty");
        for rule in self
            .success_asserts
            .iter()
            .chain(self.failed_asserts.iter())
        {
            rule.validate()?;
        }
        for rule in &self.extract_variables {
            ensure!(
                !rule.name.trim().is_empty(),
                "extract variable name is empty"
            );
            rule.rule.validate()?;
        }
        Ok(())
    }

    pub fn control(&self) -> Option<QdControl<'_>> {
        let value = self.request.url.trim();
        let statement = value.strip_prefix("{%")?.strip_suffix("%}")?.trim();
        if statement.starts_with("for ") {
            Some(QdControl::For(value))
        } else if statement.starts_with("while ") {
            Some(QdControl::While(value))
        } else if statement.starts_with("if ") {
            Some(QdControl::If(value))
        } else if statement == "else" {
            Some(QdControl::Else)
        } else if matches!(statement, "endfor" | "endwhile" | "endif") {
            Some(QdControl::End(value))
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QdControl<'a> {
    For(&'a str),
    While(&'a str),
    If(&'a str),
    Else,
    End(&'a str),
}

#[derive(Clone, Debug)]
pub struct QdProgram {
    pub blocks: Vec<QdBlock>,
}

#[derive(Clone, Debug)]
pub enum QdBlock {
    Request(Box<QdHarEntry>),
    For {
        target: String,
        source: String,
        body: Vec<QdBlock>,
    },
    While {
        condition: String,
        body: Vec<QdBlock>,
    },
    If {
        condition: String,
        then_blocks: Vec<QdBlock>,
        else_blocks: Vec<QdBlock>,
    },
}

impl QdProgram {
    pub fn compile(har: &QdHar) -> Result<Self> {
        let entries = har.enabled_entries().cloned().collect::<Vec<_>>();
        let mut cursor = 0;
        let blocks = parse_blocks(&entries, &mut cursor, None)?;
        ensure!(cursor == entries.len(), "unexpected trailing control entry");
        Ok(Self { blocks })
    }
}

fn parse_blocks(
    entries: &[QdHarEntry],
    cursor: &mut usize,
    end: Option<&str>,
) -> Result<Vec<QdBlock>> {
    let mut blocks = Vec::new();
    while let Some(entry) = entries.get(*cursor) {
        let Some(statement) = control_statement(&entry.request.url) else {
            blocks.push(QdBlock::Request(Box::new(entry.clone())));
            *cursor += 1;
            continue;
        };
        if statement == "else" || statement.starts_with("end") {
            if end.is_some() {
                break;
            }
            anyhow::bail!(
                "unexpected control tag at entry {}: {statement}",
                *cursor + 1
            );
        }

        *cursor += 1;
        if let Some(condition) = statement.strip_prefix("if ") {
            ensure!(!condition.trim().is_empty(), "if condition is empty");
            let then_blocks = parse_blocks(entries, cursor, Some("endif"))?;
            let mut else_blocks = Vec::new();
            if current_statement(entries, *cursor) == Some("else") {
                *cursor += 1;
                else_blocks = parse_blocks(entries, cursor, Some("endif"))?;
            }
            consume_end(entries, cursor, "endif")?;
            blocks.push(QdBlock::If {
                condition: condition.trim().into(),
                then_blocks,
                else_blocks,
            });
        } else if let Some(expression) = statement.strip_prefix("for ") {
            let (target, source) = expression
                .split_once(" in ")
                .context("for control must use `for <name> in <expression>`")?;
            ensure!(!target.trim().is_empty(), "for target is empty");
            ensure!(!source.trim().is_empty(), "for source is empty");
            let body = parse_blocks(entries, cursor, Some("endfor"))?;
            consume_end(entries, cursor, "endfor")?;
            blocks.push(QdBlock::For {
                target: target.trim().into(),
                source: source.trim().into(),
                body,
            });
        } else if let Some(condition) = statement.strip_prefix("while ") {
            ensure!(!condition.trim().is_empty(), "while condition is empty");
            let body = parse_blocks(entries, cursor, Some("endwhile"))?;
            consume_end(entries, cursor, "endwhile")?;
            blocks.push(QdBlock::While {
                condition: condition.trim().into(),
                body,
            });
        } else {
            blocks.push(QdBlock::Request(Box::new(entry.clone())));
        }
    }
    if let Some(expected) = end {
        let actual = current_statement(entries, *cursor);
        ensure!(
            actual == Some(expected) || (expected == "endif" && actual == Some("else")),
            "expected {expected}, found {}",
            actual.unwrap_or("end of HAR")
        );
    }
    Ok(blocks)
}

fn control_statement(value: &str) -> Option<&str> {
    value
        .trim()
        .strip_prefix("{%")?
        .strip_suffix("%}")
        .map(str::trim)
}

fn current_statement(entries: &[QdHarEntry], cursor: usize) -> Option<&str> {
    control_statement(&entries.get(cursor)?.request.url)
}

fn consume_end(entries: &[QdHarEntry], cursor: &mut usize, expected: &str) -> Result<()> {
    let actual = current_statement(entries, *cursor).unwrap_or("end of HAR");
    ensure!(actual == expected, "expected {expected}, found {actual}");
    *cursor += 1;
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QdHarRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<QdNameValue>,
    #[serde(default)]
    pub cookies: Vec<QdNameValue>,
    #[serde(rename = "postData")]
    pub post_data: Option<QdPostData>,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QdNameValue {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub checked: bool,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QdPostData {
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    pub text: Option<String>,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QdRule {
    pub re: String,
    pub from: String,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}

impl QdRule {
    fn validate(&self) -> Result<()> {
        ensure!(!self.from.trim().is_empty(), "rule source is empty");
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QdExtractRule {
    pub name: String,
    #[serde(flatten)]
    pub rule: QdRule,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_qd_extensions_and_preserves_raw_document() {
        let raw = json!({
            "log": {
                "version": "1.2",
                "creator": {"name": "binux", "version": "QD"},
                "entries": [{
                    "checked": true,
                    "custom_extension": {"keep": true},
                    "request": {
                        "method": "POST",
                        "url": "https://example.invalid/login",
                        "headers": [{"name": "X-Test", "value": "{{token}}", "checked": true}],
                        "cookies": [],
                        "postData": {"mimeType": "application/json", "text": "{}"}
                    },
                    "success_asserts": [{"re": "200", "from": "status"}],
                    "failed_asserts": [],
                    "extract_variables": [{"name": "token", "re": "token=(.+)", "from": "content"}]
                }]
            }
        });
        let har = QdHar::parse(raw.clone()).unwrap();
        assert_eq!(har.raw(), &raw);
        assert_eq!(har.enabled_entries().count(), 1);
        assert_eq!(har.entries()[0].extract_variables[0].name, "token");
        assert!(har.entries()[0].extensions.contains_key("custom_extension"));
    }

    #[test]
    fn recognizes_qd_control_entries() {
        let raw = json!({"log": {"version": "1.2", "entries": [{
            "checked": true,
            "request": {"method": "GET", "url": "{% if token %}"}
        }]}});
        let har = QdHar::parse(raw).unwrap();
        assert_eq!(
            har.entries()[0].control(),
            Some(QdControl::If("{% if token %}"))
        );
    }

    #[test]
    fn accepts_control_whitespace_and_ignores_disabled_rule_errors() {
        let raw = json!({"log": {"version": "1.2", "entries": [{
            "checked": true,
            "request": {"method": "GET", "url": "{%   while token   %}"}
        }, {
            "checked": false,
            "request": {"method": "GET", "url": "https://example.invalid"},
            "extract_variables": [{"name": "", "re": "", "from": ""}]
        }]}});
        let har = QdHar::parse(raw).unwrap();
        assert_eq!(
            har.entries()[0].control(),
            Some(QdControl::While("{%   while token   %}"))
        );
    }

    #[test]
    fn compiles_nested_qd_control_entries() {
        let urls = [
            "{% for item in items %}",
            "{% if item %}",
            "https://example.invalid/{{item}}",
            "{% else %}",
            "https://example.invalid/empty",
            "{% endif %}",
            "{% endfor %}",
        ];
        let entries = urls
            .into_iter()
            .map(|url| {
                json!({
                    "checked": true,
                    "request": {"method": "GET", "url": url}
                })
            })
            .collect::<Vec<_>>();
        let har = QdHar::parse(json!({"log": {"version": "1.2", "entries": entries}})).unwrap();
        let program = QdProgram::compile(&har).unwrap();
        let QdBlock::For { body, .. } = &program.blocks[0] else {
            panic!("expected for block");
        };
        let QdBlock::If {
            then_blocks,
            else_blocks,
            ..
        } = &body[0]
        else {
            panic!("expected if block");
        };
        assert_eq!(then_blocks.len(), 1);
        assert_eq!(else_blocks.len(), 1);
    }

    #[test]
    fn rejects_mismatched_qd_control_end() {
        let har = QdHar::parse(json!({"log": {"version": "1.2", "entries": [{
            "checked": true,
            "request": {"method": "GET", "url": "{% if token %}"}
        }, {
            "checked": true,
            "request": {"method": "GET", "url": "{% endfor %}"}
        }]}}))
        .unwrap();
        let error = QdProgram::compile(&har).unwrap_err().to_string();
        assert!(error.contains("expected endif"));
    }
}
