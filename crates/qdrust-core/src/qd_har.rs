use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct QdHar {
    raw: Value,
    document: HarDocument,
}

impl QdHar {
    pub fn parse(raw: Value) -> Result<Self> {
        let document: HarDocument =
            serde_json::from_value(raw.clone()).context("invalid QD HAR document")?;
        Self::finish_parse(raw, document)
    }

    /// Parse a QD-exported template, normalizing the shapes QD tolerates but
    /// the HAR reader does not: a bare entry array without the `log` wrapper,
    /// `request.data` string bodies, request-level `mimeType`, nested `rule`
    /// objects, missing `checked` flags, and missing HAR versions. The
    /// original input is preserved in `raw()`.
    pub fn parse_qd(raw: Value) -> Result<Self> {
        let normalized = normalize_qd_document(raw.clone());
        let document: HarDocument =
            serde_json::from_value(normalized).context("invalid QD HAR document")?;
        Self::finish_parse(raw, document)
    }

    fn finish_parse(raw: Value, document: HarDocument) -> Result<Self> {
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

/// Normalize the shapes QD exports and its template library tolerate:
/// - a bare JSON array of entries without the `{"log": ...}` wrapper;
/// - `request.data` string bodies and request-level `mimeType` (QD template
///   format) mapped onto standard HAR `postData`;
/// - `postData.params` turned into a form body when `text` is absent;
/// - a missing HAR version defaulted to 1.2.
fn normalize_qd_document(raw: Value) -> Value {
    let entries = match &raw {
        Value::Array(items) => items.clone(),
        Value::Object(object) => match object.get("log") {
            Some(Value::Object(log)) => match log.get("entries") {
                Some(Value::Array(items)) => items.clone(),
                _ => return raw,
            },
            _ => return raw,
        },
        _ => return raw,
    };
    let mut entries = entries;
    for entry in entries.iter_mut().filter_map(Value::as_object_mut) {
        // QD exports omit `checked`; every exported entry is enabled.
        entry.entry("checked").or_insert_with(|| Value::Bool(true));
        // QD nests the rules under `rule`; hoist them to the entry top level
        // where the reader expects them, so extraction and asserts survive
        // subscription/library imports that skip the UI flattening.
        if let Some(rule) = entry.remove("rule")
            && let Some(rule) = rule.as_object()
        {
            for key in ["success_asserts", "failed_asserts", "extract_variables"] {
                if let Some(value) = rule.get(key)
                    && !entry.contains_key(key)
                {
                    entry.insert(key.to_string(), value.clone());
                }
            }
        }
        let Some(request) = entry.get_mut("request").and_then(Value::as_object_mut) else {
            continue;
        };
        let data = request
            .get("data")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mime_type = request
            .get("mimeType")
            .and_then(Value::as_str)
            .map(str::to_string);
        let has_params = request
            .get("postData")
            .and_then(|post| post.get("params"))
            .is_some_and(Value::is_array);
        if data.is_none() && mime_type.is_none() && !has_params {
            continue;
        }
        let post_data = request
            .entry("postData")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(post_object) = post_data.as_object_mut() {
            if let Some(text) = data
                && !post_object.contains_key("text")
            {
                post_object.insert("text".to_string(), Value::String(text));
            }
            if let Some(mime_type) = mime_type
                && !post_object.contains_key("mimeType")
            {
                post_object.insert("mimeType".to_string(), Value::String(mime_type));
            }
            // Standard HAR exports carry form bodies as a params array; turn
            // them into the raw text the executor sends verbatim.
            if !post_object.contains_key("text")
                && let Some(Value::Array(params)) = post_object.get("params")
            {
                let body = params
                    .iter()
                    .filter_map(|param| {
                        let object = param.as_object()?;
                        let name = object.get("name")?.as_str()?;
                        let value = object.get("value").and_then(Value::as_str).unwrap_or("");
                        Some((name, value))
                    })
                    .map(|(name, value)| format!("{name}={}", form_url_encode(value)))
                    .collect::<Vec<_>>()
                    .join("&");
                post_object.insert("text".to_string(), Value::String(body));
            }
        }
    }
    json!({
        "log": {
            "version": "1.2",
            "creator": {"name": "qdrust", "version": "1.0"},
            "entries": entries,
        }
    })
}

/// Percent-encode a form value (RFC 3986 unreserved characters stay literal).
fn form_url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
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
    fn parse_qd_hoists_nested_rule_and_defaults_checked() {
        // QD template-library exports: nested `rule` object, no `checked`.
        let raw = json!([
            {
                "comment": "登录",
                "request": {
                    "method": "GET",
                    "url": "https://example.invalid/start",
                    "headers": [],
                    "cookies": []
                },
                "rule": {
                    "success_asserts": [{"re": "302", "from": "status"}],
                    "failed_asserts": [],
                    "extract_variables": [{"name": "token", "re": "token=(.+)", "from": "content"}]
                }
            }
        ]);
        let har = QdHar::parse_qd(raw).expect("QD template-library export must parse");
        let entry = &har.entries()[0];
        assert!(entry.checked, "entries without checked are enabled");
        assert_eq!(entry.success_asserts.len(), 1);
        assert_eq!(entry.extract_variables.len(), 1);
        assert_eq!(entry.extract_variables[0].name, "token");
    }

    #[test]
    fn parse_qd_normalizes_bare_array_with_data_bodies() {
        // Exactly the 189天翼云 shape: QD exports a bare entry array whose
        // POST bodies live in request.data and mimeType sits at request level.
        let raw = json!([
            {
                "checked": true,
                "comment": "登录",
                "request": {
                    "method": "POST",
                    "url": "https://open.e.189.cn/api/logbox/oauth2/loginSubmit.do",
                    "headers": [],
                    "cookies": [],
                    "mimeType": "application/x-www-form-urlencoded",
                    "data": "version=v2.0&userName={{userkey}}"
                }
            },
            {
                "checked": true,
                "request": {
                    "method": "POST",
                    "url": "https://open.e.189.cn/api/logbox/oauth2/needcaptcha.do",
                    "headers": [],
                    "data": "accountType=01"
                }
            }
        ]);
        let har = QdHar::parse_qd(raw.clone()).expect("bare-array QD tpl must parse");
        let requests = har
            .entries()
            .iter()
            .map(|entry| &entry.request)
            .collect::<Vec<_>>();
        assert_eq!(har.raw(), &raw, "original document must be preserved");
        assert_eq!(requests.len(), 2);
        let login = &requests[0];
        assert_eq!(
            login.post_data.as_ref().unwrap().mime_type.as_deref(),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(
            login.post_data.as_ref().unwrap().text.as_deref(),
            Some("version=v2.0&userName={{userkey}}")
        );
        let captcha = &requests[1];
        assert_eq!(
            captcha.post_data.as_ref().unwrap().text.as_deref(),
            Some("accountType=01")
        );
    }

    #[test]
    fn parse_qd_synthesizes_text_from_post_params() {
        let raw = json!({"log": {"version": "1.2", "entries": [{
            "checked": true,
            "request": {
                "method": "POST",
                "url": "https://example.invalid/form",
                "postData": {"params": [
                    {"name": "user", "value": "a b&c"},
                    {"name": "flag", "value": "1"}
                ]}
            }
        }]}});
        let har = QdHar::parse_qd(raw).unwrap();
        let post_data = har.entries()[0].request.post_data.as_ref().unwrap();
        assert_eq!(post_data.text.as_deref(), Some("user=a%20b%26c&flag=1"));
    }

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
