use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use minijinja::Environment;
use regex::{Regex, RegexBuilder};
use reqwest::{Client, Method, cookie::Jar, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    expression::QdExpressionEngine,
    plugin::{PluginRegistry, UtilityPlugin},
    qd_har::{QdBlock, QdHarEntry, QdHarRequest, QdNameValue, QdPostData, QdProgram, QdRule},
    template::{RequestBody, RequestStep, Step, TemplateDefinition},
};

const DEFAULT_REQUEST_LIMIT: usize = 100;
const DEFAULT_RESPONSE_LIMIT: usize = 5 * 1024 * 1024;
const MAX_LOOP_ITERATIONS: usize = 1_000;

#[derive(Clone, Default)]
pub struct CancellationToken {
    state: Arc<(std::sync::atomic::AtomicBool, tokio::sync::Notify)>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.state
            .0
            .store(true, std::sync::atomic::Ordering::Release);
        self.state.1.notify_waiters();
    }

    async fn cancelled(&self) {
        let notified = self.state.1.notified();
        if self.state.0.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

#[derive(Clone, Debug)]
pub struct ExecutorOptions {
    pub timeout: Duration,
    pub response_limit: usize,
    pub allow_private_network: bool,
    pub allow_invalid_certificates: bool,
    pub plugin_timeout: Duration,
    pub request_limit: usize,
    pub loop_limit: usize,
    /// Optional HTTP/SOCKS5 proxy URL applied to outbound requests.
    pub proxy: Option<String>,
}

impl Default for ExecutorOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            response_limit: DEFAULT_RESPONSE_LIMIT,
            allow_private_network: false,
            allow_invalid_certificates: false,
            plugin_timeout: Duration::from_secs(30),
            request_limit: DEFAULT_REQUEST_LIMIT,
            loop_limit: MAX_LOOP_ITERATIONS,
            proxy: None,
        }
    }
}

#[derive(Debug)]
pub struct ExecutionContext {
    pub variables: BTreeMap<String, Value>,
    pub remaining_requests: usize,
    pub loop_limit: usize,
    pub last_status: Option<u16>,
    pub last_headers: Vec<(String, String)>,
    pub last_body: String,
}

impl ExecutionContext {
    pub fn new(variables: BTreeMap<String, Value>) -> Self {
        Self {
            variables,
            remaining_requests: DEFAULT_REQUEST_LIMIT,
            loop_limit: MAX_LOOP_ITERATIONS,
            last_status: None,
            last_headers: Vec::new(),
            last_body: String::new(),
        }
    }

    pub fn with_limits(
        variables: BTreeMap<String, Value>,
        request_limit: usize,
        loop_limit: usize,
    ) -> Self {
        Self {
            variables,
            remaining_requests: request_limit,
            loop_limit,
            last_status: None,
            last_headers: Vec::new(),
            last_body: String::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StepResult {
    pub url: String,
    pub status: u16,
    pub body_size: usize,
}

pub struct QdExecutor {
    cookies: Arc<Jar>,
    timeout: Duration,
    allow_invalid_certificates: bool,
    templates: Environment<'static>,
    expressions: QdExpressionEngine,
    response_limit: usize,
    allow_private_network: bool,
    plugins: PluginRegistry,
    plugin_timeout: Duration,
    request_limit: usize,
    loop_limit: usize,
    proxy: Option<reqwest::Proxy>,
}

impl QdExecutor {
    pub fn new(timeout: Duration) -> Result<Self> {
        Self::with_options(ExecutorOptions {
            timeout,
            ..ExecutorOptions::default()
        })
    }

    pub fn with_options(options: ExecutorOptions) -> Result<Self> {
        ensure!(options.request_limit > 0, "request limit must be positive");
        ensure!(options.loop_limit > 0, "loop limit must be positive");
        let mut plugins = PluginRegistry::default();
        plugins.register(std::sync::Arc::new(UtilityPlugin::default()))?;
        let proxy = options
            .proxy
            .as_deref()
            .map(reqwest::Proxy::all)
            .transpose()
            .context("invalid proxy URL")?;
        Ok(Self {
            cookies: Arc::new(Jar::default()),
            timeout: options.timeout,
            allow_invalid_certificates: options.allow_invalid_certificates,
            templates: Environment::new(),
            expressions: QdExpressionEngine::default(),
            response_limit: options.response_limit,
            allow_private_network: options.allow_private_network,
            plugins,
            plugin_timeout: options.plugin_timeout,
            request_limit: options.request_limit,
            loop_limit: options.loop_limit,
            proxy,
        })
    }

    pub async fn execute(
        &self,
        program: &QdProgram,
        context: &mut ExecutionContext,
    ) -> Result<Vec<StepResult>> {
        context.remaining_requests = context.remaining_requests.min(self.request_limit);
        context.loop_limit = context.loop_limit.min(self.loop_limit);
        let mut results = Vec::new();
        self.execute_blocks(&program.blocks, context, &mut results)
            .await?;
        Ok(results)
    }

    pub async fn execute_with_deadline(
        &self,
        program: &QdProgram,
        context: &mut ExecutionContext,
        deadline: Duration,
    ) -> Result<Vec<StepResult>> {
        tokio::time::timeout(deadline, self.execute(program, context))
            .await
            .context("execution deadline exceeded")?
    }

    pub async fn execute_with_cancellation(
        &self,
        program: &QdProgram,
        context: &mut ExecutionContext,
        cancellation: &CancellationToken,
    ) -> Result<Vec<StepResult>> {
        tokio::select! {
            result = self.execute(program, context) => result,
            _ = cancellation.cancelled() => bail!("execution cancelled"),
        }
    }

    pub async fn execute_template_with_cancellation(
        &self,
        definition: &TemplateDefinition,
        context: &mut ExecutionContext,
        cancellation: &CancellationToken,
    ) -> Result<Vec<StepResult>> {
        tokio::select! {
            result = self.execute_template(definition, context) => result,
            _ = cancellation.cancelled() => bail!("execution cancelled"),
        }
    }

    pub async fn execute_template(
        &self,
        definition: &TemplateDefinition,
        context: &mut ExecutionContext,
    ) -> Result<Vec<StepResult>> {
        definition.validate()?;
        for (name, value) in &definition.variables {
            context
                .variables
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }
        context.remaining_requests = context.remaining_requests.min(self.request_limit);
        let mut results = Vec::new();
        self.execute_template_steps(&definition.steps, context, &mut results)
            .await?;
        Ok(results)
    }

    fn execute_template_steps<'a>(
        &'a self,
        steps: &'a [Step],
        context: &'a mut ExecutionContext,
        results: &'a mut Vec<StepResult>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            for step in steps {
                match step {
                    Step::Request(request) => {
                        let entry = native_request_entry(request, context, &self.templates)?;
                        results.push(self.execute_request(&entry, context).await?);
                    }
                    Step::Extract(extract_step) => {
                        let value = match extract_step.source {
                            crate::template::ExtractSource::Status => context
                                .last_status
                                .map(|status| Value::Number(status.into())),
                            crate::template::ExtractSource::Header => context
                                .last_headers
                                .iter()
                                .find(|(name, _)| name.eq_ignore_ascii_case(&extract_step.selector))
                                .map(|(_, value)| Value::String(value.clone())),
                            crate::template::ExtractSource::Text => {
                                extract(&extract_step.selector, &context.last_body)?
                            }
                            crate::template::ExtractSource::Json => {
                                let json: Value = serde_json::from_str(&context.last_body)
                                    .context("last response is not valid JSON")?;
                                json.pointer(&extract_step.selector).cloned()
                            }
                        };
                        if let Some(value) = value {
                            context.variables.insert(extract_step.target.clone(), value);
                        } else if extract_step.required {
                            bail!("required extraction did not match: {}", extract_step.name);
                        }
                    }
                    Step::If {
                        condition,
                        then,
                        otherwise,
                    } => {
                        let selected = if self
                            .expressions
                            .evaluate_bool(condition, &context.variables)?
                        {
                            then
                        } else {
                            otherwise
                        };
                        self.execute_template_steps(selected, context, results)
                            .await?;
                    }
                    Step::ForEach { item, items, steps } => {
                        let value = match context.variables.get(items).cloned() {
                            Some(value) => value,
                            None => self.expressions.evaluate(items, &context.variables)?,
                        };
                        let values = iterable_values(value)?;
                        ensure!(
                            values.len() <= context.loop_limit,
                            "for_each iteration limit exceeded"
                        );
                        for value in values {
                            context.variables.insert(item.clone(), value);
                            self.execute_template_steps(steps, context, results).await?;
                        }
                    }
                    Step::Delay { milliseconds } => {
                        tokio::time::sleep(Duration::from_millis(*milliseconds)).await;
                    }
                }
            }
            Ok(())
        })
    }

    fn execute_blocks<'a>(
        &'a self,
        blocks: &'a [QdBlock],
        context: &'a mut ExecutionContext,
        results: &'a mut Vec<StepResult>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            for block in blocks {
                match block {
                    QdBlock::Request(entry) => {
                        results.push(self.execute_request(entry, context).await?);
                    }
                    QdBlock::If {
                        condition,
                        then_blocks,
                        else_blocks,
                    } => {
                        let selected = if self
                            .expressions
                            .evaluate_bool(condition, &context.variables)?
                        {
                            then_blocks
                        } else {
                            else_blocks
                        };
                        self.execute_blocks(selected, context, results).await?;
                    }
                    QdBlock::For {
                        target,
                        source,
                        body,
                    } => {
                        let value = match context.variables.get(source).cloned() {
                            Some(value) => value,
                            None => self.expressions.evaluate(source, &context.variables)?,
                        };
                        let values = iterable_values(value)?;
                        ensure!(
                            values.len() <= context.loop_limit,
                            "for loop iteration limit exceeded"
                        );
                        enter_loop(&mut context.variables);
                        let length = values.len();
                        for (index, value) in values.into_iter().enumerate() {
                            context.variables.insert(target.clone(), value);
                            set_loop_variables(&mut context.variables, index, length);
                            self.execute_blocks(body, context, results).await?;
                        }
                        leave_loop(&mut context.variables);
                    }
                    QdBlock::While { condition, body } => {
                        enter_loop(&mut context.variables);
                        for index in 0..context.loop_limit {
                            set_loop_variables(&mut context.variables, index, context.loop_limit);
                            if !self
                                .expressions
                                .evaluate_bool(condition, &context.variables)?
                            {
                                break;
                            }
                            self.execute_blocks(body, context, results).await?;
                            if index + 1 == context.loop_limit {
                                bail!("while loop iteration limit exceeded");
                            }
                        }
                        leave_loop(&mut context.variables);
                    }
                }
            }
            Ok(())
        })
    }

    async fn execute_request(
        &self,
        entry: &QdHarEntry,
        context: &mut ExecutionContext,
    ) -> Result<StepResult> {
        ensure!(context.remaining_requests > 0, "request limit exceeded");
        context.remaining_requests -= 1;

        let method = self.render(&entry.request.method, context)?;
        let mut url = self.render(&entry.request.url, context)?;
        if url.starts_with("api://") {
            // QD 兼容：api:// 请求的 POST 表单体（如 util/urldecode 的 content=...）
            // 并入查询串后交给插件，与 QD 后端 get_argument 同时读取 query 与表单体的行为一致。
            if method.eq_ignore_ascii_case("POST")
                && let Some(post_data) = entry.request.post_data.as_ref()
            {
                let mime = post_data.mime_type.as_deref().unwrap_or("");
                if mime.is_empty() || mime.contains("application/x-www-form-urlencoded") {
                    if let Some(text) = post_data.text.as_ref() {
                        let body = self.render(text, context)?;
                        url = merge_form_into_query(&url, &body);
                    }
                }
            }
            let response = self.plugins.call(&url, self.plugin_timeout).await?;
            return self.finish_response(
                entry,
                context,
                url,
                response.status,
                response.headers.into_iter().collect(),
                response.body,
            );
        }
        let client = self.client_for_url(&url).await?;
        let method = Method::from_bytes(method.as_bytes()).context("invalid rendered method")?;
        let mut request = client.request(method, &url);
        for header in entry.request.headers.iter().filter(|header| header.checked) {
            request = request.header(
                self.render(&header.name, context)?,
                self.render(&header.value, context)?,
            );
        }
        let cookies = entry
            .request
            .cookies
            .iter()
            .filter(|cookie| cookie.checked)
            .map(|cookie| {
                Ok(format!(
                    "{}={}",
                    self.render(&cookie.name, context)?,
                    self.render(&cookie.value, context)?
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        if !cookies.is_empty() {
            request = request.header(reqwest::header::COOKIE, cookies.join("; "));
        }
        if let Some(post_data) = entry.request.post_data.as_ref() {
            let is_multipart = post_data
                .mime_type
                .as_deref()
                .is_some_and(|m| m.starts_with("multipart/"));
            if is_multipart {
                request = request.multipart(self.build_multipart(post_data, context)?);
            } else if let Some(text) = post_data.text.as_ref() {
                request = request.body(self.render(text, context)?);
            }
        }

        let response = request.send().await?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = response.bytes().await?.to_vec();
        self.finish_response(entry, context, url, status, headers, body)
    }

    fn finish_response(
        &self,
        entry: &QdHarEntry,
        context: &mut ExecutionContext,
        url: String,
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<StepResult> {
        ensure!(
            body.len() <= self.response_limit,
            "response body limit exceeded"
        );
        let content = String::from_utf8_lossy(&body);

        context.last_status = Some(status);
        context.last_headers = headers.clone();
        context.last_body = content.to_string();

        self.check_rules(
            &entry.success_asserts,
            true,
            status,
            &headers,
            &content,
            context,
        )?;
        self.check_rules(
            &entry.failed_asserts,
            false,
            status,
            &headers,
            &content,
            context,
        )?;
        for rule in &entry.extract_variables {
            let pattern = self.render(&rule.rule.re, context)?;
            let source = rule_source(&rule.rule.from, status, &headers, &content);
            if let Some(value) = extract(&pattern, &source)? {
                context.variables.insert(rule.name.clone(), value);
            }
        }

        Ok(StepResult {
            url,
            status,
            body_size: body.len(),
        })
    }

    fn check_rules(
        &self,
        rules: &[QdRule],
        success_rules: bool,
        status: u16,
        headers: &[(String, String)],
        content: &str,
        context: &ExecutionContext,
    ) -> Result<()> {
        if rules.is_empty() {
            return Ok(());
        }
        let mut matched = false;
        for rule in rules {
            let pattern = self.render(&rule.re, context)?;
            let source = rule_source(&rule.from, status, headers, content);
            if compile_regex(&pattern)?.is_match(&source) {
                matched = true;
                if !success_rules {
                    bail!("failed assertion matched: {pattern}");
                }
                break;
            }
        }
        if success_rules && !matched {
            bail!("no success assertion matched");
        }
        Ok(())
    }

    fn render(&self, value: &str, context: &ExecutionContext) -> Result<String> {
        self.templates
            .render_str(value, &context.variables)
            .context("cannot render QD template value")
    }

    fn build_multipart(
        &self,
        post_data: &QdPostData,
        context: &ExecutionContext,
    ) -> Result<reqwest::multipart::Form> {
        let mut form = reqwest::multipart::Form::new();
        if let Some(text) = post_data.text.as_ref() {
            // QD sometimes serializes a multipart body as raw text; send it verbatim.
            let body = self.render(text, context)?;
            form = form.part("body", reqwest::multipart::Part::bytes(body.into_bytes()));
            return Ok(form);
        }
        if let Some(params) = post_data
            .extensions
            .get("params")
            .and_then(|v| v.as_array())
        {
            for param in params {
                let name = self.render(
                    param.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    context,
                )?;
                if param
                    .get("fileName")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty())
                {
                    let file_name = self.render(
                        param.get("fileName").and_then(|v| v.as_str()).unwrap_or(""),
                        context,
                    )?;
                    let content = self.render(
                        param.get("value").and_then(|v| v.as_str()).unwrap_or(""),
                        context,
                    )?;
                    let raw = content.into_bytes();
                    let mut part =
                        reqwest::multipart::Part::bytes(raw.clone()).file_name(file_name.clone());
                    if let Some(mime) = param.get("contentType").and_then(|v| v.as_str())
                        && let Ok(typed) = reqwest::multipart::Part::bytes(raw)
                            .file_name(file_name)
                            .mime_str(mime)
                    {
                        part = typed;
                    }
                    form = form.part(name, part);
                } else {
                    let value = self.render(
                        param.get("value").and_then(|v| v.as_str()).unwrap_or(""),
                        context,
                    )?;
                    form = form.text(name, value);
                }
            }
        }
        Ok(form)
    }

    async fn client_for_url(&self, url: &str) -> Result<Client> {
        let (parsed, addresses) = resolve_target(url, self.allow_private_network).await?;
        let host = parsed.host_str().context("URL host is missing")?;
        let builder = Client::builder()
            .cookie_provider(self.cookies.clone())
            .redirect(Policy::none())
            .danger_accept_invalid_certs(self.allow_invalid_certificates)
            .timeout(self.timeout);
        if let Some(proxy) = self.proxy.clone() {
            // With a proxy, DNS is delegated to the proxy; do not pin the host.
            return builder
                .proxy(proxy)
                .build()
                .context("cannot build proxied HTTP client");
        }
        builder
            .resolve(host, addresses[0])
            .build()
            .context("cannot build pinned HTTP client")
    }

    #[cfg(test)]
    fn build_pinned_client(&self, host: &str, address: std::net::SocketAddr) -> Result<Client> {
        Client::builder()
            .cookie_provider(self.cookies.clone())
            .redirect(Policy::none())
            .danger_accept_invalid_certs(self.allow_invalid_certificates)
            .timeout(self.timeout)
            .resolve(host, address)
            .build()
            .context("cannot build pinned HTTP client")
    }
}

fn native_request_entry(
    request: &RequestStep,
    context: &ExecutionContext,
    templates: &Environment<'static>,
) -> Result<QdHarEntry> {
    let mut url = templates
        .render_str(&request.url, &context.variables)
        .context("cannot render template request URL")?;
    let mut parsed = reqwest::Url::parse(&url).context("invalid rendered template URL")?;
    {
        let mut query = parsed.query_pairs_mut();
        for (name, value) in &request.query {
            query.append_pair(
                &templates.render_str(name, &context.variables)?,
                &templates.render_str(value, &context.variables)?,
            );
        }
    }
    url = parsed.to_string();
    let headers = request
        .headers
        .iter()
        .map(|(name, value)| QdNameValue {
            name: name.clone(),
            value: value.clone(),
            checked: true,
            extensions: serde_json::Map::new(),
        })
        .collect();
    let post_data = request.body.as_ref().map(|body| {
        let (mime_type, text) = match body {
            RequestBody::Json(value) => (Some("application/json".into()), value.to_string()),
            RequestBody::Text(value) => (Some("text/plain".into()), value.clone()),
            RequestBody::Form(values) => (
                Some("application/x-www-form-urlencoded".into()),
                values
                    .iter()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect::<Vec<_>>()
                    .join("&"),
            ),
        };
        QdPostData {
            mime_type,
            text: Some(text),
            extensions: serde_json::Map::new(),
        }
    });
    Ok(QdHarEntry {
        checked: true,
        comment: Some(request.name.clone()),
        request: QdHarRequest {
            method: request.method.clone(),
            url,
            headers,
            cookies: Vec::new(),
            post_data,
            extensions: serde_json::Map::new(),
        },
        success_asserts: Vec::new(),
        failed_asserts: Vec::new(),
        extract_variables: Vec::new(),
        extensions: serde_json::Map::new(),
    })
}

/// 表单体值解码（与 Python parse_qsl 一致：先处理 '+'，再做百分号解码）。
fn form_urldecode(input: &str) -> String {
    percent_encoding::percent_decode_str(&input.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

/// 解析 application/x-www-form-urlencoded 请求体为键值对（QD 模板的 api:// 调用
/// 常以 POST 表单体传参，如 `content=...`）。
fn parse_form_pairs(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (name, value) = part.split_once('=').unwrap_or((part, ""));
            (form_urldecode(name), form_urldecode(value))
        })
        .collect()
}

/// 把表单键值对追加到 api:// URL 的查询串。值会被重新百分号编码，
/// 插件层 from_api_url 解码后与原值一致。
fn merge_form_into_query(url: &str, body: &str) -> String {
    let pairs = parse_form_pairs(body);
    if pairs.is_empty() {
        return url.to_string();
    }
    let mut merged = url.to_string();
    for (index, (name, value)) in pairs.iter().enumerate() {
        merged.push(if index == 0 && !url.contains('?') { '?' } else { '&' });
        merged.push_str(&percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC).to_string());
        merged.push('=');
        merged.push_str(&percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string());
    }
    merged
}

fn rule_source(source: &str, status: u16, headers: &[(String, String)], content: &str) -> String {
    match source {
        "content" => content.into(),
        "status" => status.to_string(),
        "header" => headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>()
            .join("\n"),
        value if value.starts_with("header-") => headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&value[7..]))
            .map(|(_, value)| value.clone())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn compile_regex(pattern: &str) -> Result<Regex> {
    let Some((body, flags)) = split_qd_regex(pattern) else {
        return Regex::new(pattern).context("invalid QD regular expression");
    };
    let mut builder = RegexBuilder::new(body);
    builder
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m'))
        .dot_matches_new_line(flags.contains('s'))
        .unicode(flags.contains('u'));
    builder.build().context("invalid QD regular expression")
}

fn split_qd_regex(pattern: &str) -> Option<(&str, &str)> {
    let rest = pattern.strip_prefix('/')?;
    let slash = rest.rfind('/')?;
    let (body, suffix) = rest.split_at(slash);
    let flags = &suffix[1..];
    flags
        .chars()
        .all(|flag| "gimsu".contains(flag))
        .then_some((body, flags))
}

fn extract(pattern: &str, source: &str) -> Result<Option<Value>> {
    let regex = compile_regex(pattern)?;
    let global = split_qd_regex(pattern).is_some_and(|(_, flags)| flags.contains('g'));
    if global {
        let values = regex
            .captures_iter(source)
            .map(|capture| {
                capture
                    .get(1)
                    .or_else(|| capture.get(0))
                    .map(|value| Value::String(value.as_str().into()))
                    .unwrap_or(Value::Null)
            })
            .collect();
        return Ok(Some(Value::Array(values)));
    }
    Ok(regex.captures(source).and_then(|capture| {
        capture
            .get(1)
            .or_else(|| capture.get(0))
            .map(|value| Value::String(value.as_str().into()))
    }))
}

fn iterable_values(value: Value) -> Result<Vec<Value>> {
    match value {
        Value::Array(values) => Ok(values),
        Value::Object(values) => Ok(values.keys().cloned().map(Value::String).collect()),
        Value::String(value) => Ok(value
            .chars()
            .map(|value| Value::String(value.to_string()))
            .collect()),
        _ => bail!("for expression is not iterable"),
    }
}

fn set_loop_variables(variables: &mut BTreeMap<String, Value>, index: usize, length: usize) {
    variables.insert("loop_index0".into(), Value::String(index.to_string()));
    variables.insert("loop_index".into(), Value::String((index + 1).to_string()));
    variables.insert(
        "loop_first".into(),
        Value::String(if index == 0 { "True" } else { "False" }.into()),
    );
    variables.insert(
        "loop_last".into(),
        Value::String(if index + 1 == length { "True" } else { "False" }.into()),
    );
    variables.insert("loop_length".into(), Value::String(length.to_string()));
    variables.insert(
        "loop_revindex0".into(),
        Value::String((length - index - 1).to_string()),
    );
    variables.insert(
        "loop_revindex".into(),
        Value::String((length - index).to_string()),
    );
}

async fn resolve_target(
    url: &str,
    allow_private_network: bool,
) -> Result<(reqwest::Url, Vec<std::net::SocketAddr>)> {
    let parsed = reqwest::Url::parse(url).context("invalid rendered URL")?;
    ensure!(
        matches!(parsed.scheme(), "http" | "https"),
        "unsupported URL scheme"
    );
    let host = parsed.host_str().context("URL host is missing")?;
    let port = parsed
        .port_or_known_default()
        .context("URL port is missing")?;
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .context("cannot resolve request host")?
        .collect::<Vec<_>>();
    ensure!(
        !addresses.is_empty(),
        "request host resolved to no addresses"
    );
    if !allow_private_network {
        ensure!(
            addresses.iter().all(|address| is_public_ip(address.ip())),
            "private or special-use network target is blocked"
        );
    }
    Ok((parsed, addresses))
}

fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.octets()[0] == 0)
        }
        std::net::IpAddr::V6(ip) => {
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local())
        }
    }
}

fn enter_loop(variables: &mut BTreeMap<String, Value>) {
    let depth = variables
        .get("loop_depth")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        + 1;
    variables.insert("loop_depth".into(), Value::String(depth.to_string()));
    variables.insert("loop_depth0".into(), Value::String((depth - 1).to_string()));
}

fn leave_loop(variables: &mut BTreeMap<String, Value>) {
    let depth = variables
        .get("loop_depth")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .saturating_sub(1);
    variables.insert("loop_depth".into(), Value::String(depth.to_string()));
    variables.insert(
        "loop_depth0".into(),
        Value::String(if depth == 0 {
            "-1".into()
        } else {
            (depth - 1).to_string()
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::HeaderMap, response::IntoResponse, routing::get};
    use serde_json::json;

    use crate::qd_har::{QdHar, QdProgram};

    fn local_executor() -> QdExecutor {
        QdExecutor::with_options(ExecutorOptions {
            timeout: Duration::from_secs(5),
            allow_private_network: true,
            ..ExecutorOptions::default()
        })
        .unwrap()
    }

    #[tokio::test]
    async fn executes_requests_with_rendering_cookies_assertions_and_extraction() {
        let app = Router::new()
            .route(
                "/first",
                get(|| async { ([("set-cookie", "session=ready; Path=/")], "token=abc123") }),
            )
            .route(
                "/second",
                get(|headers: HeaderMap| async move {
                    let cookie = headers
                        .get("cookie")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default();
                    if cookie.contains("session=ready") {
                        ([("x-result", "accepted")], "used=abc123").into_response()
                    } else {
                        (axum::http::StatusCode::UNAUTHORIZED, "missing cookie").into_response()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let har = QdHar::parse(json!({"log": {"version": "1.2", "entries": [{
            "checked": true,
            "request": {"method": "GET", "url": format!("http://{address}/first")},
            "success_asserts": [{"re": "200", "from": "status"}],
            "extract_variables": [{"name": "token", "re": "token=(.+)", "from": "content"}]
        }, {
            "checked": true,
            "request": {"method": "GET", "url": format!("http://{address}/second?token={{{{token}}}}")},
            "success_asserts": [{"re": "accepted", "from": "header-x-result"}],
            "failed_asserts": [{"re": "missing", "from": "content"}]
        }]}}))
        .unwrap();
        let program = QdProgram::compile(&har).unwrap();
        let executor = local_executor();
        let mut context = ExecutionContext::new(BTreeMap::new());

        let results = executor.execute(&program, &mut context).await.unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[1].status, 200);
        assert!(results[1].url.ends_with("token=abc123"));
        assert_eq!(context.variables.get("token"), Some(&json!("abc123")));
        assert_eq!(context.remaining_requests, DEFAULT_REQUEST_LIMIT - 2);
    }

    #[tokio::test]
    async fn executes_if_and_for_control_flow() {
        let app = Router::new().route("/{item}", get(|| async { "ok" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let control = |url: &str| {
            json!({
                "checked": true,
                "request": {"method": "GET", "url": url}
            })
        };
        let har = QdHar::parse(json!({"log": {"version": "1.2", "entries": [
            control("{% if enabled %}"),
            control("{% for item in range(1, 4) %}"),
            control(&format!("http://{address}/{{{{item}}}}")),
            control("{% endfor %}"),
            control("{% endif %}")
        ]}}))
        .unwrap();
        let program = QdProgram::compile(&har).unwrap();
        let executor = local_executor();
        let mut context = ExecutionContext::new(BTreeMap::from([("enabled".into(), json!(true))]));

        let results = executor.execute(&program, &mut context).await.unwrap();

        assert_eq!(results.len(), 3);
        assert!(results[0].url.ends_with("/1"));
        assert!(results[2].url.ends_with("/3"));
        assert_eq!(context.variables.get("loop_index"), Some(&json!("3")));
        assert_eq!(context.variables.get("loop_last"), Some(&json!("True")));
        assert_eq!(context.variables.get("loop_depth"), Some(&json!("0")));
        assert_eq!(context.variables.get("loop_depth0"), Some(&json!("-1")));
    }

    #[tokio::test]
    async fn executes_native_template_requests_and_extractions() {
        let app = Router::new().route(
            "/hello",
            get(|headers: HeaderMap| async move {
                let query = headers
                    .get("x-query")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                (
                    [
                        ("x-result", query),
                        ("content-type", "application/json".to_owned()),
                    ],
                    r#"{"token":"abc"}"#,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let definition = TemplateDefinition {
            version: 1,
            name: "native smoke".into(),
            variables: BTreeMap::from([(String::from("query"), json!("accepted"))]),
            steps: vec![
                Step::Request(RequestStep {
                    name: "hello".into(),
                    method: "GET".into(),
                    url: format!("http://{address}/hello"),
                    headers: BTreeMap::from([(String::from("x-query"), "{{query}}".into())]),
                    query: BTreeMap::new(),
                    body: None,
                }),
                Step::Extract(crate::template::ExtractStep {
                    name: "token".into(),
                    source: crate::template::ExtractSource::Json,
                    selector: "/token".into(),
                    target: "token".into(),
                    required: true,
                }),
                Step::Extract(crate::template::ExtractStep {
                    name: "result header".into(),
                    source: crate::template::ExtractSource::Header,
                    selector: "x-result".into(),
                    target: "result".into(),
                    required: true,
                }),
                Step::If {
                    condition: "token == 'abc'".into(),
                    then: vec![Step::Delay { milliseconds: 0 }],
                    otherwise: vec![],
                },
            ],
        };
        let executor = local_executor();
        let mut context = ExecutionContext::new(BTreeMap::new());
        let results = executor
            .execute_template(&definition, &mut context)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, 200);
        assert_eq!(context.variables.get("token"), Some(&json!("abc")));
        assert_eq!(context.variables.get("result"), Some(&json!("accepted")));
    }

    #[tokio::test]
    async fn limits_infinite_while_loop() {
        let program = QdProgram {
            blocks: vec![QdBlock::While {
                condition: "true".into(),
                body: Vec::new(),
            }],
        };
        let executor = QdExecutor::new(Duration::from_secs(1)).unwrap();
        let mut context = ExecutionContext::new(BTreeMap::new());

        let error = executor
            .execute(&program, &mut context)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("while loop iteration limit exceeded"));
    }

    #[tokio::test]
    async fn applies_configured_request_and_loop_limits() {
        let har = QdHar::parse(json!({"log": {"version": "1.2", "entries": [
            {"checked": true, "request": {"method": "GET", "url": "api://util/delay?seconds=0"}},
            {"checked": true, "request": {"method": "GET", "url": "api://util/delay?seconds=0"}}
        ]}}))
        .unwrap();
        let program = QdProgram::compile(&har).unwrap();
        let executor = QdExecutor::with_options(ExecutorOptions {
            request_limit: 1,
            loop_limit: 2,
            ..ExecutorOptions::default()
        })
        .unwrap();
        let mut context = ExecutionContext::new(BTreeMap::new());
        let error = executor.execute(&program, &mut context).await.unwrap_err();
        assert!(error.to_string().contains("request limit exceeded"));
        assert_eq!(context.remaining_requests, 0);
    }

    #[tokio::test]
    async fn aborts_execution_at_deadline() {
        let har = QdHar::parse(json!({"log": {"version": "1.2", "entries": [
            {"checked": true, "request": {"method": "GET", "url": "api://util/delay?seconds=1"}}
        ]}}))
        .unwrap();
        let program = QdProgram::compile(&har).unwrap();
        let executor = QdExecutor::new(Duration::from_secs(5)).unwrap();
        let mut context = ExecutionContext::new(BTreeMap::new());
        let error = executor
            .execute_with_deadline(&program, &mut context, Duration::from_millis(20))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("execution deadline exceeded"));
    }

    #[tokio::test]
    async fn cancels_execution_tree() {
        let har = QdHar::parse(json!({"log": {"version": "1.2", "entries": [
            {"checked": true, "request": {"method": "GET", "url": "api://util/delay?seconds=1"}}
        ]}}))
        .unwrap();
        let program = QdProgram::compile(&har).unwrap();
        let executor = QdExecutor::new(Duration::from_secs(5)).unwrap();
        let mut context = ExecutionContext::new(BTreeMap::new());
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = executor
            .execute_with_cancellation(&program, &mut context, &cancellation)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("execution cancelled"));
    }

    #[tokio::test]
    async fn blocks_private_targets_by_default() {
        let error = resolve_target("http://127.0.0.1:8080", false)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("private or special-use"));
        resolve_target("http://127.0.0.1:8080", true).await.unwrap();
    }

    #[tokio::test]
    async fn pins_hostname_to_validated_socket_address() {
        let app = Router::new().route("/pinned", get(|| async { "pinned" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let executor = local_executor();
        let client = executor
            .build_pinned_client("does-not-resolve.invalid", address)
            .unwrap();

        let body = client
            .get(format!(
                "http://does-not-resolve.invalid:{}/pinned",
                address.port()
            ))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert_eq!(body, "pinned");
    }

    #[tokio::test]
    async fn executes_qd_api_plugin_through_normal_rules() {
        let har = QdHar::parse(json!({"log": {"version": "1.2", "entries": [{
            "checked": true,
            "request": {"method": "GET", "url": "api://util/delay?seconds=0"},
            "success_asserts": [{"re": "200", "from": "status"}],
            "extract_variables": [{"name": "delay_result", "re": "(delayed .+)", "from": "content"}]
        }]}}))
        .unwrap();
        let program = QdProgram::compile(&har).unwrap();
        let executor = QdExecutor::new(Duration::from_secs(1)).unwrap();
        let mut context = ExecutionContext::new(BTreeMap::new());

        let results = executor.execute(&program, &mut context).await.unwrap();

        assert_eq!(results[0].status, 200);
        assert_eq!(
            context.variables.get("delay_result"),
            Some(&json!("delayed 0 seconds"))
        );
    }

    #[tokio::test]
    async fn executes_qd_urldecode_with_post_form_body() {
        // 复刻 QD 模板常见写法：POST api://util/urldecode，content 走表单体，
        // 断言 "状态": "200" 并用 "转换后": "(.*)" 提取 __log__。
        let har = QdHar::parse(json!({"log": {"version": "1.2", "entries": [{
            "checked": true,
            "comment": "生成日志",
            "request": {
                "method": "POST",
                "url": "api://util/urldecode",
                "headers": [],
                "cookies": [],
                "postData": {"mimeType": "", "text": "content=7li7li签到：获得{{points}}积分{{error}}"}
            },
            "success_asserts": [
                {"re": "200", "from": "status"},
                {"re": "\"状态\": \"200\"", "from": "content"}
            ],
            "extract_variables": [{"name": "__log__", "re": "\"转换后\": \"(.*)\"", "from": "content"}]
        }]}}))
        .unwrap();
        let program = QdProgram::compile(&har).unwrap();
        let executor = QdExecutor::new(Duration::from_secs(1)).unwrap();
        let mut context = ExecutionContext::new(BTreeMap::from([("points".into(), json!("20"))]));

        let results = executor.execute(&program, &mut context).await.unwrap();

        assert_eq!(results[0].status, 200);
        assert_eq!(
            context.variables.get("__log__"),
            Some(&json!("7li7li签到：获得20积分"))
        );
    }

    #[test]
    fn supports_qd_regex_flags_and_global_extraction() {
        assert!(compile_regex("/^hello/im").unwrap().is_match("x\nHello"));
        assert_eq!(
            extract("/id=(\\d+)/g", "id=1 id=2").unwrap(),
            Some(json!(["1", "2"]))
        );
    }
}
