use std::{
    borrow::Cow,
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, LazyLock},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use fancy_regex::{Regex as FancyRegex, RegexBuilder as FancyRegexBuilder};
use regex::{Captures, Regex, RegexBuilder};
use reqwest::{
    Client, Method,
    cookie::{CookieStore, Jar},
    redirect::Policy,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    expression::QdExpressionEngine,
    plugin::{Plugin, PluginRegistry, UtilityPlugin},
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

impl ExecutorOptions {
    /// The part of these options that decides whether a request may happen at
    /// all, in the form [`guarded_client_for_url`] takes.
    ///
    /// Separate from `ExecutorOptions` because the request/loop/plugin limits
    /// are meaningless to an outbound fetch that is not a template run — the
    /// server makes those fetches too, and handing it the whole options struct
    /// would invite it to think the limits applied.
    pub fn outbound_policy(&self) -> OutboundPolicy {
        OutboundPolicy {
            timeout: self.timeout,
            allow_private_network: self.allow_private_network,
            allow_invalid_certificates: self.allow_invalid_certificates,
            proxy: self.proxy.clone(),
        }
    }
}

/// How one outbound request is allowed to be made.
///
/// The two `allow_*` switches are the ADR-0008 relaxations and default off;
/// everything the SSRF guard reads lives here, so a caller either states its
/// whole posture or takes [`OutboundPolicy::default`].
#[derive(Clone, Debug)]
pub struct OutboundPolicy {
    pub timeout: Duration,
    pub allow_private_network: bool,
    pub allow_invalid_certificates: bool,
    /// Optional HTTP/SOCKS5 proxy URL applied to outbound requests.
    pub proxy: Option<String>,
}

impl Default for OutboundPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            allow_private_network: false,
            allow_invalid_certificates: false,
            proxy: None,
        }
    }
}

#[derive(Debug)]
pub struct ExecutionContext {
    pub variables: BTreeMap<String, Value>,
    pub remaining_requests: usize,
    pub loop_limit: usize,
    /// The HTTP status of the last response, or `None` if no response ever
    /// arrived — a TLS, DNS or timeout failure — and before the first request.
    ///
    /// The two are deliberately not merged with a sentinel: a template can read
    /// this through `extract.from: status`, where "no status" and "status 0"
    /// would be different answers. Whichever caller persists it must pair it
    /// with the error, because `None` alone does not mean "healthy" — see
    /// `record_run` and the WebUI's `taskOutcome`.
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
    /// Everything the SSRF guard reads, in one place. Held as the policy rather
    /// than as loose `timeout`/`allow_*`/`proxy` fields so the executor and the
    /// built-in `util` plugin cannot drift apart: they are handed the same
    /// value at construction, which is what keeps the plugin's DdddOCR forward
    /// under the same gate as every request the executor makes itself.
    policy: OutboundPolicy,
    expressions: QdExpressionEngine,
    response_limit: usize,
    plugins: PluginRegistry,
    plugin_timeout: Duration,
    request_limit: usize,
    loop_limit: usize,
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
        let policy = options.outbound_policy();
        let mut plugins = PluginRegistry::default();
        // The built-in plugin is handed the same policy. Its DdddOCR forward
        // leaves this process, and a second copy of the switches is exactly how
        // that forward ends up unguarded while everything else is guarded.
        plugins.register(Arc::new(UtilityPlugin::with_policy(policy.clone())))?;
        Ok(Self {
            cookies: Arc::new(Jar::default()),
            policy,
            expressions: QdExpressionEngine::default(),
            response_limit: options.response_limit,
            plugins,
            plugin_timeout: options.plugin_timeout,
            request_limit: options.request_limit,
            loop_limit: options.loop_limit,
        })
    }

    /// Wire a plugin into this executor. Templates reach it through
    /// `api://<plugin-id>/<action>` and its response flows through the normal
    /// success/failed assertions and `extract_variables`, exactly like the
    /// built-in `util` plugin. Registering nothing keeps today's behaviour.
    pub fn register_plugin(&mut self, plugin: Arc<dyn Plugin>) -> Result<()> {
        self.plugins.register(plugin)
    }

    /// Ids of every registered plugin, for run-log diagnostics.
    pub fn plugin_ids(&self) -> Vec<String> {
        self.plugins.ids()
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
                        let entry = native_request_entry(request, context, &self.expressions)?;
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
        let debug_requests = std::env::var("QDRUST_DEBUG_REQUESTS")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        if debug_requests {
            eprintln!(
                "[qdrust:request:start] method={} url={} variables={}",
                method,
                url,
                context
                    .variables
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
        if url.starts_with("api://") {
            // QD 兼容：api:// 请求的 POST 表单体（如 util/urldecode 的 content=...）
            // 并入查询串后交给插件，与 QD 后端 get_argument 同时读取 query 与表单体的行为一致。
            if method.eq_ignore_ascii_case("POST")
                && let Some(post_data) = entry.request.post_data.as_ref()
            {
                let mime = post_data.mime_type.as_deref().unwrap_or("");
                if (mime.is_empty() || mime.contains("application/x-www-form-urlencoded"))
                    && let Some(text) = post_data.text.as_ref()
                {
                    let body = self.render(text, context)?;
                    if debug_requests {
                        eprintln!("[qdrust:request:api-body] url={} body={}", url, body);
                    }
                    url = merge_form_into_query(&url, &body);
                }
            }
            let response = self.plugins.call(&url, self.plugin_timeout).await?;
            if debug_requests {
                eprintln!(
                    "[qdrust:request:api-response] url={} status={} body={}",
                    url,
                    response.status,
                    bounded_preview(&String::from_utf8_lossy(&response.body))
                );
            }
            return self.finish_response(
                entry,
                context,
                url,
                &request_digest(&method, None),
                response.status,
                response.headers.into_iter().collect(),
                response.body,
            );
        }
        let client = self.client_for_url(&url, context).await?;
        let method = Method::from_bytes(method.as_bytes()).context("invalid rendered method")?;
        let request_method = method.clone();
        let mut request = client.request(method, &url);
        let mut rendered_body: Option<String> = None;
        let mut explicit_cookie = false;
        let mut rendered_cookie_header: Option<String> = None;
        for header in entry.request.headers.iter().filter(|header| header.checked) {
            let rendered_name = self.render(&header.name, context)?;
            // HTTP/2 pseudo-headers are metadata from the browser capture, not
            // legal request headers for reqwest. The URL/method already carry
            // their meaning.
            if rendered_name.starts_with(':') {
                continue;
            }
            let rendered_value = self.render(&header.value, context)?;
            // reqwest is built without compression decoders; request identity
            // responses so extracted content remains readable for all HARs.
            if rendered_name.eq_ignore_ascii_case("accept-encoding") {
                request = request.header(reqwest::header::ACCEPT_ENCODING, "identity");
                continue;
            }
            if rendered_name.eq_ignore_ascii_case("cookie") {
                explicit_cookie = true;
                rendered_cookie_header = Some(rendered_value.clone());
            }
            if debug_requests {
                eprintln!(
                    "[qdrust:request:header] url={} {}={}",
                    url, rendered_name, rendered_value
                );
            }
            request = request.header(rendered_name, rendered_value);
        }
        if let Some(cookie_header) = rendered_cookie_header {
            context.variables.insert(
                "__qdrust_cookie_header".into(),
                Value::String(cookie_header),
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
            let cookie_header = cookies.join("; ");
            request = request.header(reqwest::header::COOKIE, &cookie_header);
            context.variables.insert(
                "__qdrust_cookie_header".into(),
                Value::String(cookie_header),
            );
            // HAR files often define cookies only on the first request and rely
            // on the browser jar for subsequent requests. Mirror them into the
            // reqwest jar so later entries inherit the authenticated session.
            let cookie_url = reqwest::Url::parse(&url).context("invalid cookie URL")?;
            for cookie in &cookies {
                // Jar accepts Set-Cookie syntax; HAR stores a Cookie header.
                let set_cookie = format!("{cookie}; Path=/");
                self.cookies.add_cookie_str(&set_cookie, &cookie_url);
            }
        }
        if !explicit_cookie {
            if let Some(Value::String(cookie_header)) =
                context.variables.get("__qdrust_cookie_header")
            {
                request = request.header(reqwest::header::COOKIE, cookie_header);
            }
            if let Some(value) = self.cookies.cookies(&reqwest::Url::parse(&url)?) {
                request = request.header(reqwest::header::COOKIE, value);
            }
        }
        if let Some(post_data) = entry.request.post_data.as_ref() {
            let mime_type = post_data.mime_type.as_deref().unwrap_or("");
            let has_content_type = entry
                .request
                .headers
                .iter()
                .filter(|header| header.checked)
                .any(|header| header.name.eq_ignore_ascii_case("content-type"));
            if !mime_type.is_empty() && !has_content_type {
                // Browsers add this header when submitting a form even when
                // older QD HAR exports omit it (notably loginSubmit.do).
                request = request.header(reqwest::header::CONTENT_TYPE, mime_type);
                if debug_requests {
                    eprintln!(
                        "[qdrust:request:header:auto] url={} Content-Type={}",
                        url, mime_type
                    );
                }
            }
            let is_multipart = post_data
                .mime_type
                .as_deref()
                .is_some_and(|m| m.starts_with("multipart/"));
            if is_multipart {
                request = request.multipart(self.build_multipart(post_data, context)?);
            } else if let Some(text) = post_data.text.as_ref() {
                // Render once: the same body goes onto the wire and into the
                // assertion-failure digest, so log diagnosis sees what was sent.
                let rendered = self.render(text, context)?;
                let body = if post_data.mime_type.as_deref().is_some_and(|mime| {
                    mime.to_ascii_lowercase()
                        .contains("application/x-www-form-urlencoded")
                }) {
                    encode_form_body(&rendered)
                } else {
                    rendered
                };
                if debug_requests {
                    eprintln!(
                        "[qdrust:request:body] url={} content_type={} body={}",
                        url,
                        post_data.mime_type.as_deref().unwrap_or(""),
                        body
                    );
                }
                rendered_body = Some(body.clone());
                request = request.body(body);
            }
        }

        let response = request.send().await?;
        let status = response.status().as_u16();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        if debug_requests {
            for (name, value) in &headers {
                if name.eq_ignore_ascii_case("location") || name.eq_ignore_ascii_case("set-cookie")
                {
                    eprintln!(
                        "[qdrust:request:response-header] url={} {}={}",
                        url, name, value
                    );
                }
            }
        }
        let body = response.bytes().await?.to_vec();
        if debug_requests {
            eprintln!(
                "[qdrust:request:response] url={} status={} body={}",
                url,
                status,
                bounded_preview(&String::from_utf8_lossy(&body))
            );
        }
        self.finish_response(
            entry,
            context,
            url,
            &request_digest(request_method.as_str(), rendered_body.as_deref()),
            status,
            headers,
            body,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_response(
        &self,
        entry: &QdHarEntry,
        context: &mut ExecutionContext,
        url: String,
        request_digest: &str,
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<StepResult> {
        ensure!(
            body.len() <= self.response_limit,
            "response body limit exceeded"
        );
        // QD decodes the body once and hands every rule the same text; for an
        // image it hands the base64 of the raw bytes instead, so a captcha
        // survives an assertion or an extraction (see `is_image_response`).
        let content = if is_image_response(&headers) {
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &body)
        } else {
            decode_body(&body, &headers)
        };

        context.last_status = Some(status);
        context.last_headers = headers.clone();
        context.last_body = content.clone();

        // QD shows the step request and response in the run log; surface both
        // so site-side rejections (e.g. result:-1 "用户名或密码为空") are
        // diagnosable from the failure message alone.
        let response_preview = bounded_preview(&content);
        let request_digest = request_digest.to_string();
        let url = url.clone();
        let with_response = |cause: anyhow::Error| {
            anyhow::anyhow!(
                "{cause} (at {url})\nrequest: {request_digest}\nresponse: {response_preview}"
            )
        };
        self.check_rules(
            &entry.success_asserts,
            true,
            status,
            &headers,
            &content,
            context,
        )
        .map_err(with_response)?;
        self.check_rules(
            &entry.failed_asserts,
            false,
            status,
            &headers,
            &content,
            context,
        )
        .map_err(with_response)?;
        for rule in &entry.extract_variables {
            // QD uses an empty extraction expression as a no-op placeholder.
            if rule.rule.re.is_empty() {
                continue;
            }
            let pattern = self.render(&rule.rule.re, context)?;
            let source = rule_source(&rule.rule.from, status, &headers, &content);
            // QD compiles and searches inside a try/except and stores the
            // exception's text as the variable's value, so a pattern its engine
            // refuses leaves the run running: the mistake arrives as a variable
            // holding the message rather than as a dead task, and a template
            // that guards with `|default` still finishes. The message is this
            // engine's own, which is what makes the variable useful to read.
            let extracted = match extract(&pattern, &source) {
                Ok(extracted) => extracted,
                Err(err) => Some(Value::String(err.to_string())),
            };
            if debug_requests_enabled() {
                let preview = extracted
                    .as_ref()
                    .map(|value| bounded_preview(&value.to_string()))
                    .unwrap_or_else(|| "<no-match>".into());
                eprintln!(
                    "[qdrust:extract] name={} from={} result={}",
                    rule.name, rule.rule.from, preview
                );
            }
            if let Some(value) = extracted {
                context.variables.insert(rule.name.clone(), value);
            }
        }

        // The 189.cn login flow returns HTTP 200 even when appConf cannot
        // produce the required parameters. Stop here with a useful message
        // instead of allowing a later loginSubmit request to report the
        // misleading "用户名或密码为空" error.
        if url.contains("/oauth2/appConf.do") && content.contains("\"data\":{}") {
            bail!("QD appConf returned empty data (check reqid, lt and referer headers)");
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
            // QD hands an assertion straight to `re.search`, delimiters and
            // flags included, where a `/…/g` string would only ever match
            // itself — no template in qd-today/templates writes one that way
            // (all 3456 assertions are bare patterns), so reading the flags
            // here is a superset of what QD does rather than a difference.
            //
            // The `?` also covers the fallback engine giving up, which is an
            // error rather than a "no match": an assertion that could not be
            // evaluated has not failed, and reporting it as one would be a
            // verdict nothing reached.
            if compile_regex(&pattern)?.is_match(&source)? {
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
        // Route through QdExpressionEngine so every QD global function and
        // filter (urlencode, a2b_base64, ...) is available during rendering.
        self.expressions
            .render(value, &context.variables)
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

    /// The part of this executor that decides whether a request may happen at
    /// all, in the form [`guarded_client_for_url`] takes.
    ///
    async fn client_for_url(&self, url: &str, context: &ExecutionContext) -> Result<Client> {
        let policy = self.policy_with_proxy(context);
        guarded_client_for_url(url, &policy, Some(self.cookies.clone())).await
    }

    /// The run policy with this step's `_proxy` applied.
    ///
    /// QD reads `env["variables"]["_proxy"]` before every request
    /// (`libs/fetcher.py::do_fetch`), so a template can send one run through a
    /// proxy without a server-wide setting — the reason it exists is a
    /// trawl/flaresolverr in front of a Cloudflare challenge. An empty or
    /// absent value means "no proxy", which also clears whatever the run policy
    /// started with; the guard still classifies the proxy host (see
    /// [`classify_proxy`]), so this cannot be used to reach loopback or
    /// metadata addresses while the private-network switch is off.
    fn policy_with_proxy(&self, context: &ExecutionContext) -> OutboundPolicy {
        let mut policy = self.policy.clone();
        let proxy = context
            .variables
            .get("_proxy")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        policy.proxy = (!proxy.is_empty()).then(|| proxy.to_string());
        policy
    }

    #[cfg(test)]
    fn build_pinned_client(&self, host: &str, address: std::net::SocketAddr) -> Result<Client> {
        Client::builder()
            .cookie_provider(self.cookies.clone())
            .redirect(Policy::none())
            .danger_accept_invalid_certs(self.policy.allow_invalid_certificates)
            .timeout(self.policy.timeout)
            .resolve(host, address)
            .build()
            .context("cannot build pinned HTTP client")
    }
}

fn native_request_entry(
    request: &RequestStep,
    context: &ExecutionContext,
    templates: &QdExpressionEngine,
) -> Result<QdHarEntry> {
    let mut url = templates
        .render(&request.url, &context.variables)
        .context("cannot render template request URL")?;
    let mut parsed = reqwest::Url::parse(&url).context("invalid rendered template URL")?;
    {
        let mut query = parsed.query_pairs_mut();
        for (name, value) in &request.query {
            query.append_pair(
                &templates.render(name, &context.variables)?,
                &templates.render(value, &context.variables)?,
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

/// Collapse whitespace and cap a text preview for failure diagnostics.
fn debug_requests_enabled() -> bool {
    std::env::var("QDRUST_DEBUG_REQUESTS")
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn bounded_preview(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview: String = collapsed.chars().take(300).collect();
    if collapsed.chars().count() > 300 {
        preview.push('…');
    }
    preview
}

/// One-line digest of what a step actually sent, for assertion-failure logs.
fn request_digest(method: &str, body: Option<&str>) -> String {
    match body {
        Some(body) => bounded_preview(&format!("{method} body: {body}")),
        None => bounded_preview(method),
    }
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

/// Encode rendered form fields without allowing '&' or '=' inside a value to
/// become new fields. Existing percent escapes are decoded first so values are
/// not double-encoded (important for QD's chained urlencode expressions).
fn encode_form_body(body: &str) -> String {
    body.split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (name, value) = part.split_once('=').unwrap_or((part, ""));
            let encode = |value: &str| {
                percent_encoding::utf8_percent_encode(&form_urldecode(value), FORM_VALUE_ENCODE_SET)
                    .to_string()
            };
            format!("{}={}", encode(name), encode(value))
        })
        .collect::<Vec<_>>()
        .join("&")
}

const FORM_VALUE_ENCODE_SET: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b'&')
    .add(b'=')
    .add(b'?')
    .add(b'#')
    .add(b'%')
    .add(b'+')
    .add(b' ');

/// 把表单键值对追加到 api:// URL 的查询串。值会被重新百分号编码，
/// 插件层 from_api_url 解码后与原值一致。
fn merge_form_into_query(url: &str, body: &str) -> String {
    let pairs = parse_form_pairs(body);
    if pairs.is_empty() {
        return url.to_string();
    }
    let mut merged = url.to_string();
    for (index, (name, value)) in pairs.iter().enumerate() {
        merged.push(if index == 0 && !url.contains('?') {
            '?'
        } else {
            '&'
        });
        merged.push_str(
            &percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC)
                .to_string(),
        );
        merged.push('=');
        merged.push_str(
            &percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC)
                .to_string(),
        );
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

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Whether a response says it is an image.
///
/// QD's `run_rule.getdata` hands `content` to a rule as the base64 of the raw
/// bytes when the response is one, and that is how a template keeps a captcha
/// image intact through an assertion or an extraction.
fn is_image_response(headers: &[(String, String)]) -> bool {
    header_value(headers, "content-type")
        .is_some_and(|value| value.to_ascii_lowercase().contains("image"))
}

/// The `charset` parameter of a `Content-Type` value, quotes stripped.
fn charset_parameter(content_type: &str) -> Option<&str> {
    let lowered = content_type.to_ascii_lowercase();
    let start = lowered.find("charset")? + "charset".len();
    let rest = content_type[start..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.trim_start_matches(['"', '\'']);
    let end = rest
        .find(|character: char| {
            character == ';' || character == ',' || character == '"' || character == '\''
        })
        .unwrap_or(rest.len());
    let charset = rest[..end].trim();
    (!charset.is_empty()).then_some(charset)
}

/// `charset` declarations a body carries: `<meta charset=…>`,
/// `<meta content="…;charset=…">` and an XML declaration, in that order.
///
/// The three patterns are QD's `get_encodings_from_content` (`libs/utils.py`),
/// which runs them over the raw bytes and takes the first match. What they look
/// for is ASCII, so reading the bytes lossily turns exactly those declarations
/// into text without altering them.
fn declared_in_body(body: &[u8]) -> Option<String> {
    static META_CHARSET: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)<meta.*?charset=["']*(.+?)["'>]"#).unwrap());
    static META_PRAGMA: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)<meta.*?content=["']*;?charset=(.+?)["'>]"#).unwrap());
    // No case-insensitive flag, as in QD, and `^` is the start of the document
    // rather than a line, which is Python's reading without `re.M`.
    static XML_DECLARATION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"^<\?xml.*?encoding=["']*(.+?)["'>]"#).unwrap());

    for pattern in [&META_CHARSET, &META_PRAGMA, &XML_DECLARATION] {
        if let Some(captured) = pattern.captures(&String::from_utf8_lossy(body)) {
            return captured.get(1).map(|value| value.as_str().to_string());
        }
    }
    None
}

/// The encoding an encoding label names, as `encoding_rs` knows it.
fn encoding_for(label: &str) -> Option<&'static encoding_rs::Encoding> {
    let label = label.trim_matches(|character: char| {
        character == '"' || character == '\'' || character.is_whitespace()
    });
    if label.eq_ignore_ascii_case("unicode") {
        // QD has a literal label with a meaning of its own: `unicode` means the
        // bytes are already text, which for a `str` is UTF-8.
        return Some(encoding_rs::UTF_8);
    }
    if label.eq_ignore_ascii_case("gb2312") {
        // QD upgrades this one: gb2312 is what sites *say*, gb18030 is what
        // their bytes actually use once a character outside the 1980 set
        // appears.
        return Some(encoding_rs::GB18030);
    }
    if label.eq_ignore_ascii_case("latin_1") {
        return Some(encoding_rs::WINDOWS_1252);
    }
    encoding_rs::Encoding::for_label(label.as_bytes())
}

/// The encoding to read a response body with, from the sources QD's
/// `utils.find_encoding` reads.
///
/// Every source and every normalization is kept: the `charset` a response
/// header declares, a declaration inside the body, an automatic detection when
/// neither says anything, the `ISO-8859-1` a header carries by default
/// *discarded* (it is what an HTTP stack writes when nobody decided, so QD
/// treats it as silence), `gb2312` upgraded to `gb18030`, and Latin-1 as the
/// last resort.
///
/// One step is ordered differently on purpose. QD asks its detector
/// (`charset_normalizer`) *before* it reads the body's declaration, but that
/// ordering only matters when the detector says nothing, and there it reports
/// `utf_8` for anything ASCII or UTF-8 — so in QD the declaration is consulted
/// mostly for the legacy encodings a detector has to guess at. This build's
/// detector is `chardetng` (the one Firefox uses, from the same lineage as the
/// `encoding_rs` decoders here), and swapping one guess for another is not
/// fidelity to anything: a declaration in the page is a fact, so it is read
/// before any guess. With no declaration the two do the same job.
fn find_encoding(body: &[u8], headers: &[(String, String)]) -> &'static encoding_rs::Encoding {
    if let Some(charset) = header_value(headers, "content-type")
        .and_then(charset_parameter)
        .filter(|charset| !charset.eq_ignore_ascii_case("iso-8859-1"))
        && let Some(encoding) = encoding_for(charset)
    {
        return encoding;
    }
    if let Some(charset) = declared_in_body(body)
        && let Some(encoding) = encoding_for(&charset)
    {
        return encoding;
    }
    // ISO-2022-JP is denied (the setting for content that can run scripts, and
    // a character set no template here reads), and UTF-8 is allowed to be the
    // guess: `charset_normalizer` answers `utf_8` for unlabelled UTF-8, and a
    // page that is UTF-8 without saying so has to decode as UTF-8 to be read at
    // all.
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(body, true);
    detector.guess(None, chardetng::Utf8Detection::Allow)
}

/// A response body as text, decoded the way QD decodes it.
///
/// `errors="replace"` is QD's, and `decode_without_bom_handling` is what gives
/// it here: a byte the encoding cannot carry becomes U+FFFD rather than failing
/// the step, and a byte-order mark stays in the text exactly as Python's
/// `bytes.decode` leaves it.
fn decode_body(body: &[u8], headers: &[(String, String)]) -> String {
    let (text, _) = find_encoding(body, headers).decode_without_bom_handling(body);
    text.into_owned()
}

/// How much backtracking the fallback engine may do before it gives up.
///
/// Rust's `regex` is linear-time and cannot be made to run long by any pattern,
/// so it needs no such knob. `fancy-regex` backtracks, which is what buys
/// lookaround, and that is also what makes `^((?!msg).)*$` over a long line
/// exponential. The budget is what turns that into "the engine gave up"
/// instead of a hung executor.
///
/// It is written out rather than left to the crate's default so that a
/// template's behaviour cannot change under us in a dependency bump. The
/// number matches the crate's own `MAX_STACK` and its default limit, and it is
/// the same order as the largest response a step may see
/// ([`DEFAULT_RESPONSE_LIMIT`]), so a plausible match is reached well inside it
/// while a runaway one is cut off.
const BACKTRACK_LIMIT: usize = 1_000_000;

/// A QD pattern, compiled by whichever engine can take it.
///
/// Almost everything QD writes compiles on Rust's `regex`, which does not
/// backtrack and therefore cannot be driven into exponential time by any
/// pattern. Lookaround and back-references are the exception — Python's `re`
/// has them, `regex` does not — so those fall through to `fancy-regex`, which
/// backtracks and is built with [`BACKTRACK_LIMIT`].
///
/// The linear engine is tried first, and the second engine is only ever reached
/// for a pattern the first one refuses, so the backtracking path stays as narrow
/// as QD's dialect requires it to be.
///
/// The fallback variant keeps the pattern as the template wrote it, because
/// that engine can also fail at match time and the error then has to name what
/// it gave up on.
enum CompiledRegex {
    Linear(Regex),
    Backtracking {
        regex: Box<FancyRegex>,
        pattern: String,
    },
}

impl CompiledRegex {
    fn is_match(&self, source: &str) -> Result<bool> {
        match self {
            Self::Linear(regex) => Ok(regex.is_match(source)),
            Self::Backtracking { regex, pattern } => backtracked(pattern, regex.is_match(source)),
        }
    }

    /// Every match, each shaped the way Python's `re.findall` shapes it.
    fn find_all(&self, source: &str) -> Result<Vec<Value>> {
        match self {
            Self::Linear(regex) => Ok(regex
                .captures_iter(source)
                .map(|capture| qd_findall_entry(&capture))
                .collect()),
            Self::Backtracking { regex, pattern } => {
                let mut values = Vec::new();
                for capture in regex.captures_iter(source) {
                    values.push(qd_findall_entry(&backtracked(pattern, capture)?));
                }
                Ok(values)
            }
        }
    }

    fn first(&self, source: &str) -> Result<Option<Value>> {
        match self {
            Self::Linear(regex) => Ok(regex.captures(source).as_ref().and_then(qd_first_value)),
            Self::Backtracking { regex, pattern } => {
                Ok(backtracked(pattern, regex.captures(source))?
                    .as_ref()
                    .and_then(qd_first_value))
            }
        }
    }
}

/// A fallback-engine result, with "I gave up" turned into an error that names
/// the pattern.
///
/// Reporting `false` instead would be a verdict the engine never reached, and
/// the two limits it can hit — backtracking steps and stack depth — are exactly
/// the cases where QD's unguarded Python would have kept going. Saying so is
/// more useful than a guess, and points at the pattern to rewrite.
fn backtracked<T>(pattern: &str, result: fancy_regex::Result<T>) -> Result<T> {
    result.with_context(|| {
        format!("QD regular expression {pattern:?} gave up: it needed more backtracking than the fallback engine allows ({BACKTRACK_LIMIT} steps)")
    })
}

/// The slice of a capture set that QD's two return shapes need — how many
/// groups there are, and the text of one of them — so that the code reading
/// them can be written once for both engines, whose capture types are otherwise
/// unrelated.
trait QdCaptures {
    fn group_count(&self) -> usize;
    fn group(&self, index: usize) -> Option<&str>;
}

impl QdCaptures for Captures<'_> {
    fn group_count(&self) -> usize {
        self.len()
    }

    fn group(&self, index: usize) -> Option<&str> {
        self.get(index).map(|value| value.as_str())
    }
}

impl QdCaptures for fancy_regex::Captures<'_, str> {
    fn group_count(&self) -> usize {
        self.len()
    }

    fn group(&self, index: usize) -> Option<&str> {
        self.get(index).map(|value| value.as_str())
    }
}

/// The flags a delimited QD pattern can carry.
///
/// qd-today reads them itself in `libs/fetcher.py`: `re.match(r"^/(.*?)/([gimsu]*)$", …)`
/// and then one `flags |=` per letter. `g` is the odd one out — it is not a
/// Python `re` flag but "match all", and only `extract_variables` looks at it.
#[derive(Clone, Copy, Debug, Default)]
struct QdFlags {
    /// Python's `re.findall` rather than `re.search`.
    global: bool,
    /// `i`
    case_insensitive: bool,
    /// `m`
    multi_line: bool,
    /// `s`
    dot_matches_new_line: bool,
}

impl QdFlags {
    /// `u` is accepted and changes nothing: Python compiles a `str` pattern as
    /// Unicode unless `re.A` overrides it, and `re.U` is a no-op on Python 3,
    /// so `u` can only ever restate the default. It never asks for ASCII or
    /// byte semantics — mapping it to `RegexBuilder::unicode(false)`, as this
    /// code once did, is what gave every `/(.+?)/g` in every template the
    /// error "pattern can match invalid UTF-8" (issue #26).
    fn parse(flags: &str) -> Self {
        Self {
            global: flags.contains('g'),
            case_insensitive: flags.contains('i'),
            multi_line: flags.contains('m'),
            dot_matches_new_line: flags.contains('s'),
        }
    }

    /// Compile `body` into the matches Python's `re` would report for these
    /// flags, keeping `pattern` — what the template wrote — for the failures
    /// the fallback engine can still report at match time.
    ///
    /// The linear engine gets first refusal. Only when it will not take the
    /// pattern does the backtracking one, which is the path lookaround and
    /// back-references need; if that refuses too, the pattern is one Python
    /// refuses as well (`[^]`, a variable-width look-behind) and the linear
    /// engine's complaint — the narrower of the two — is the one reported.
    ///
    /// The error is a plain string because the three ways to fail have
    /// different error types: the translation refuses a `{2,1}`, the engines
    /// refuse everything neither of them can read, and a conditional group can
    /// branch on a group the pattern does not have — which only the compiled
    /// pattern knows.
    fn build(self, pattern: &str, body: &str) -> std::result::Result<CompiledRegex, String> {
        let translated = translate_from_python(body)?;
        match self.linear(&translated.body) {
            Ok(regex) => {
                translated.check_conditions(regex.capture_names(), regex.captures_len())?;
                Ok(CompiledRegex::Linear(regex))
            }
            Err(error) => match self.backtracking(&translated.body) {
                Ok(regex) => {
                    translated.check_conditions(regex.capture_names(), regex.captures_len())?;
                    Ok(CompiledRegex::Backtracking {
                        regex: Box::new(regex),
                        pattern: pattern.to_string(),
                    })
                }
                Err(_) => Err(error.to_string()),
            },
        }
    }

    fn linear(self, body: &str) -> std::result::Result<Regex, regex::Error> {
        let mut builder = RegexBuilder::new(body);
        builder
            .case_insensitive(self.case_insensitive)
            .multi_line(self.multi_line)
            .dot_matches_new_line(self.dot_matches_new_line)
            // Always on, and deliberately not wired to the `u` flag: Unicode
            // is Rust's default too, so the two engines agree on everything QD
            // can write.
            .unicode(true);
        builder.build()
    }

    /// The same flags again on the backtracking engine.
    ///
    /// `unicode_mode` is this builder's spelling of the `unicode(true)` above.
    /// The look-behind feature is deliberately left off: Python's `re` requires
    /// a fixed-width look-behind, and the templates' look-behinds are all
    /// fixed-width, so turning the feature on could only make this engine
    /// accept patterns QD would reject.
    ///
    /// `seek` is on. Without it this engine retries the pattern at every
    /// position of the haystack, so a `(?s)(?=.*"ok"…)` that fails costs the
    /// whole haystack per position — quadratic, and measured: Linux_SB.har's
    /// assertion over a 100 KB body that does not contain the payload takes
    /// 63.8 s, and it is *failing* on exactly the responses a scheduled run is
    /// most likely to see (an expired session's error page). `seek` derives a
    /// conservative approximation of the pattern and uses it to skip positions
    /// that cannot start a match; the same body answers in 1.3 ms.
    ///
    /// The flag is marked experimental upstream, and the note there is worth
    /// repeating: the approximation is conservative, so it may offer positions
    /// that do not match, but it must not skip one that would. That is the one
    /// property that matters — a skipped match would silently change an
    /// assertion's verdict — so it is not taken on the mechanism's word: the
    /// differential runs all 6280 cases with the flag on and off and finds no
    /// difference, and the one place it does change an answer is upward, giving
    /// the true "no match" where the engine previously gave up
    /// (see `a_runaway_pattern_gives_up_instead_of_running_away`).
    fn backtracking(self, body: &str) -> fancy_regex::Result<FancyRegex> {
        FancyRegexBuilder::new(body)
            .case_insensitive(self.case_insensitive)
            .multi_line(self.multi_line)
            .dot_matches_new_line(self.dot_matches_new_line)
            .unicode_mode(true)
            .backtrack_limit(BACKTRACK_LIMIT)
            .seek(true)
            .build()
    }
}

/// Read `body` the way Python's `re` reads it, in the places where Rust's
/// engine reads the same characters differently.
///
/// The first is an omitted repetition minimum: Python's `{,n}` is `{0,n}` and
/// `{,}` is `{0,}`, and writing the zero out is the whole difference.
///
/// The second is a literal brace. Python reads `{` as a repetition only when a
/// well-formed one follows, so `{"total":(\d+)` — how one writes an extract for
/// a JSON API — is the text `{"total":` and a group. Rust's engine insists on
/// the well-formed form and refuses the pattern instead, which is why 27 of the
/// 6182 regexes in qd-today/templates, most of them extracting from JSON, never
/// got as far as running. Escaping the brace leaves both engines matching the
/// same text, and the patterns that do use a repetition are copied through
/// untouched.
///
/// The third is `\Z`, the end of the string in Python; Rust's engine spells
/// that `\z` and rejects `\Z` outright. Python rejects `\Z` inside a class too,
/// so inside one the text is left alone and both engines refuse it.
///
/// The fourth is a backslash before punctuation, which Python reads as the
/// punctuation itself and Rust's engine refuses — see [`means_itself`].
///
/// The fifth is a `[` inside a class, which Python reads as one more member of
/// the set and Rust reads as the start of a POSIX class.
///
/// The sixth is a backslash before a digit: `\012` is the newline and `\123` is
/// `S` in Python, and a back-reference only when the digits are not one of
/// those. Both engines read every one of them as a back-reference. The digits
/// Python allows, and the values it refuses, are [`python_numbered_escape`]'s.
///
/// Beyond those it refuses constructs, in both directions.
///
/// Two are Python's syntax that the engines would misread: `{m,n}` with
/// `m > n`, which is a syntax error in Python and a match in the fallback
/// engine, and `\N{...}`, which Python reads as a named character and the
/// fallback engine reads as "not a newline".
///
/// Three are patterns Python *refuses* that the engines here would run, so that
/// a template could work here and fail in QD: `(?<name>...)` and the
/// backtracking verbs `(*FAIL)` and friends, and an inline flag turned off
/// outside a group. Refusing them is what keeps "it runs" from meaning
/// something different here than it does in QD. The flag groups are
/// [`inline_flag_group`]'s, and all three are [`refuse_unknown_group`]'s.
///
/// What it reads but does not translate is the groups, because a conditional
/// group has to branch on one: [`python_group`] refuses one that names a group
/// Python has not read yet, and leaves the group *number* to
/// [`Translated::check_conditions`], since Python lets the reference come
/// before the group it names.
fn translate_from_python(body: &str) -> std::result::Result<Translated<'_>, &'static str> {
    let characters: Vec<char> = body.chars().collect();
    let mut translated = String::with_capacity(body.len());
    let mut changed = false;
    let mut index = 0;
    let mut in_class = false;
    // A `]` that opens a class body is a literal, and so is one after `[^`.
    let mut class_head = false;
    // The capturing groups read so far, named ones included, because their
    // order is what numbers them and a conditional group may only name a group
    // Python has already read.
    let mut groups: Vec<Option<String>> = Vec::new();
    let mut conditions: Vec<(usize, Option<String>)> = Vec::new();
    while index < characters.len() {
        let character = characters[index];
        // An escape owns the character after it, `{` and `}` included.
        if character == '\\' {
            let escaped = characters.get(index + 1).copied();
            if !in_class && escaped == Some('Z') {
                translated.push_str("\\z");
                changed = true;
                index += 2;
                class_head = false;
                continue;
            }
            // `\N{BULLET}` is `•` in Python, inside a class and out, and Python
            // has no other reading of `\N`. Rust's engine refuses the escape
            // outright, which was the right answer; the fallback engine reads
            // `\N` as "not a newline" and the name as literal text, so it
            // compiles and matches the wrong thing. Reading it properly needs
            // the Unicode name table, and a refusal is the honest alternative.
            if escaped == Some('N') {
                return Err(r"`\N`, Python's named-character escape");
            }
            // A digit is a character in Python more often than the engines'
            // back-reference, which is the one reading they have.
            if escaped.is_some_and(|escaped| escaped.is_ascii_digit()) {
                let digits: Vec<char> = characters[index + 1..]
                    .iter()
                    .take_while(|character| character.is_ascii_digit())
                    .copied()
                    .collect();
                if let NumberedEscape::Octal { text, span } =
                    python_numbered_escape(&digits, in_class)?
                {
                    translated.push_str(&text);
                    changed = true;
                    index += 1 + span;
                    class_head = false;
                    continue;
                }
            }
            if let Some(literal) = escaped.filter(|escaped| means_itself(*escaped)) {
                translated.push(literal);
                changed = true;
                index += 2;
                class_head = false;
                continue;
            }
            // Any other letter is a `bad escape` in Python. Rust's engine would
            // refuse it too, but the fallback engine takes some of them as
            // oniguruma operators — `\h`, `\R`, `\G`, `\X` — so a pattern QD
            // rejects would start working here.
            if escaped.is_some_and(|escaped| !knows_escape(escaped)) {
                return Err("an escape Python does not know");
            }
            translated.push(character);
            if let Some(escaped) = escaped {
                translated.push(escaped);
                index += 2;
            } else {
                index += 1;
            }
            class_head = false;
            continue;
        }
        if in_class {
            // A class is a set of characters in both engines, braces included,
            // so there is almost nothing here to translate — except a `[`,
            // which Python reads as one more member of the set and Rust reads
            // as the start of a POSIX class. Python has no `[[:alpha:]]`, so
            // escaping the bracket is its reading in every case: `[[:alpha:]]`
            // is the six characters `[ : a l p h` and then a literal `]`.
            if character == '[' {
                translated.push_str("\\[");
                changed = true;
                index += 1;
                class_head = false;
                continue;
            }
            if character == ']' && !class_head {
                in_class = false;
            }
            if character != '^' {
                class_head = false;
            }
            translated.push(character);
            index += 1;
            continue;
        }
        if character == '[' {
            in_class = true;
            class_head = true;
            translated.push(character);
            index += 1;
            continue;
        }
        // A group open, which is where the constructs Python and the engines
        // disagree about live, and where the groups a conditional group may
        // branch on are read.
        if character == '(' {
            match python_group(&characters[index..])? {
                Group::Capturing(name) => groups.push(name),
                Group::Plain => {}
                // Python reads group 0 as the whole match, and will not take it
                // as a condition.
                Group::Conditional(Condition::Number(0)) => {
                    return Err("a conditional group branching on group 0");
                }
                Group::Conditional(Condition::Number(number)) => conditions.push((number, None)),
                Group::Conditional(Condition::Name(name)) => {
                    let Some(group) = groups
                        .iter()
                        .position(|seen| seen.as_deref() == Some(name.as_str()))
                    else {
                        return Err("a conditional group naming a group not read before it");
                    };
                    // The number for the name, because the fallback engine reads
                    // the name but never finds the group: measured,
                    // `(?P<x>a)?(?(x)b|c)` matches `bc` and not `ab`, so the
                    // `yes` branch is unreachable through a name there. The
                    // number for the same group takes it. That the number is the
                    // right one is [`Translated::check_conditions`]'s.
                    let number = group + 1;
                    translated.push_str(&format!("(?({number})"));
                    changed = true;
                    index += 4 + name.chars().count();
                    class_head = false;
                    conditions.push((number, Some(name)));
                    continue;
                }
                // An assertion or a quoted name, which Python refuses as well,
                // so the fallback engine's own complaint is the one reported.
                Group::Conditional(Condition::Other) => {}
            }
        }
        if character == '{' {
            let Some(repetition) = python_repetition(&characters[index..]) else {
                translated.push_str("\\{");
                changed = true;
                index += 1;
                continue;
            };
            // `{2,1}` is not a repetition Python reads, it is a syntax error —
            // and the fallback engine would read it as a match, so it is
            // refused here rather than left to an engine whose verdict would
            // differ from QD's.
            if repetition.minimum_exceeds_maximum {
                return Err("a repetition whose minimum is greater than its maximum");
            }
            if repetition.omitted_minimum {
                translated.push_str("{0");
                translated.extend(&characters[index + 1..index + repetition.length]);
                changed = true;
            } else {
                translated.extend(&characters[index..index + repetition.length]);
            }
            index += repetition.length;
            continue;
        }
        translated.push(character);
        index += 1;
    }
    Ok(Translated {
        body: if changed {
            Cow::Owned(translated)
        } else {
            Cow::Borrowed(body)
        },
        conditions,
    })
}

/// What the dialect layer made of a pattern: the text to compile, and the one
/// verdict it cannot reach on its own.
struct Translated<'a> {
    body: Cow<'a, str>,
    /// Every conditional group's branch target, in the order they were read: the
    /// group number, and the name it was written as when it was written as one.
    ///
    /// Whether those groups exist is what the compiled pattern knows and a
    /// reading of the pattern cannot, and Python lets the branch name a group
    /// that comes later — measured: `(?(2)a|b)(x)(y)` is legal and
    /// `(a)(?(2)b|c)` is not.
    conditions: Vec<(usize, Option<String>)>,
}

impl Translated<'_> {
    /// A conditional group may only branch on a group the pattern has, and a
    /// branch written as a name has to have been translated to the right number.
    ///
    /// The compiled pattern is what decides both. The fallback engine — which is
    /// the one that implements the construct — takes any number at all and reads
    /// a missing group as "did not take part", so a template could branch on
    /// group 2 of a one-group pattern and get an answer QD would never have
    /// produced. Its own capture names are also the only authority on whether
    /// the number a name was translated to is that name's group, which is a
    /// question this layer answers from its own reading of the pattern —
    /// `(?x)` comments and `(?#…)` are the places that reading could be wrong,
    /// and a wrong number is a wrong branch rather than an error.
    fn check_conditions<'a>(
        &self,
        names: impl Iterator<Item = Option<&'a str>>,
        captures_len: usize,
    ) -> std::result::Result<(), String> {
        if self.conditions.is_empty() {
            return Ok(());
        }
        let names: Vec<Option<&str>> = names.collect();
        for (group, name) in &self.conditions {
            // `captures_len` counts the whole match as group 0, so the groups
            // themselves are the numbers below it.
            if *group >= captures_len {
                return Err(format!(
                    "a conditional group branching on group {group}, which does not exist"
                ));
            }
            if let Some(name) = name
                && names.get(*group) != Some(&Some(name.as_str()))
            {
                return Err(format!(
                    "a conditional group naming `{name}`, which is not group {group} of the pattern"
                ));
            }
        }
        Ok(())
    }
}

/// What a `(` at the start of `characters` opens, as far as this layer cares.
enum Group {
    /// A group that captures, named or not. The order of these is what numbers
    /// them, which is what a conditional group written as a name is translated
    /// to.
    Capturing(Option<String>),
    /// A group that captures nothing: `(?:...)`, a look-around, a comment, an
    /// inline flag group, another conditional group.
    Plain,
    /// A `(?(...)yes|no)` conditional group.
    Conditional(Condition),
}

/// The group a conditional group branches on.
enum Condition {
    /// A group number, whose existence waits for the compiled pattern.
    Number(usize),
    /// A group name, which has to have been read already.
    Name(String),
    /// Neither — an assertion or a quoted name. Python refuses those as well,
    /// so the engines' own complaint is the one reported.
    Other,
}

/// Read the `(` at the start of `characters`.
///
/// The condition of a conditional group is the one thing here that has to be
/// read rather than translated. Python takes a group number or the name of a
/// group it has already read — measured: `(?(n)a|b)(?P<n>x)` is an error and
/// `(?(2)a|b)(x)(y)` is not — while the fallback engine takes anything at all,
/// numbers included. Refusing the shapes Python refuses is what keeps a
/// conditional branch from being taken where QD would not have run the pattern.
fn python_group(characters: &[char]) -> std::result::Result<Group, &'static str> {
    // `(*FAIL)` and the other backtracking verbs, which the fallback engine
    // implements. Python reads the `*` as a repetition of nothing and refuses
    // the pattern, so this is the one shape here that is not reached through a
    // `?`.
    if characters.get(1) == Some(&'*') {
        return Err("a backtracking verb, `(*FAIL)` and friends, which Python has no syntax for");
    }
    // A bare `(` is the one capturing group Python writes without a `?`.
    if characters.get(1) != Some(&'?') {
        return Ok(Group::Capturing(None));
    }
    if let Some(reason) = refuse_unknown_group(characters) {
        return Err(reason);
    }
    match characters.get(2) {
        // `(?(id)yes|no)` branches on whether another group took part in the
        // match, which the fallback engine implements and the linear one does
        // not — so a pattern with one always takes the fallback.
        Some('(') => Ok(Group::Conditional(python_condition(&characters[3..]))),
        Some('P') if characters.get(3) == Some(&'<') => Ok(python_named_group(characters)),
        _ => Ok(Group::Plain),
    }
}

/// The name of a `(?P<name>...)` group, for as long as it looks like one. A
/// `(?P<name>` that is not closed is not a named group at all, and the engines
/// refuse it on their own.
fn python_named_group(characters: &[char]) -> Group {
    let length = characters[4..]
        .iter()
        .take_while(|character| character.is_alphanumeric() || **character == '_')
        .count();
    if length == 0 || characters.get(4 + length) != Some(&'>') {
        return Group::Plain;
    }
    Group::Capturing(Some(characters[4..4 + length].iter().collect()))
}

/// The group a `(?(...)` branches on, read the way Python reads one: digits, or
/// a name, either of them closed by the `)`.
fn python_condition(characters: &[char]) -> Condition {
    let number: String = characters
        .iter()
        .take_while(|character| character.is_ascii_digit())
        .collect();
    if !number.is_empty() {
        return match characters.get(number.chars().count()) {
            // A number too large for `usize` cannot be a group either, and
            // saturating keeps it that way.
            Some(&')') => Condition::Number(number.parse().unwrap_or(usize::MAX)),
            _ => Condition::Other,
        };
    }
    let name: String = characters
        .iter()
        .take_while(|character| character.is_alphanumeric() || **character == '_')
        .collect();
    match characters.get(name.chars().count()) {
        Some(&')') if !name.is_empty() => Condition::Name(name),
        _ => Condition::Other,
    }
}

/// The `(?` forms Python reads and the engines here do not, refused by name so
/// the message says which construct it was instead of complaining about a
/// parenthesis.
///
/// The refusal is the awkward direction — a pattern Python would never run that
/// both engines accept, which was measured: `(?<n>a)` and `(*FAIL)` both
/// compile here. A template that used them would work here and fail in QD, so
/// refusing them is what keeps "it runs" meaning the same thing in both.
fn refuse_unknown_group(characters: &[char]) -> Option<&'static str> {
    match characters.get(2) {
        // A look-behind opens with the same two characters and Python reads it,
        // so only a `<` that is not `(?<=` or `(?<!` is a named group.
        Some('<') if !matches!(characters.get(3), Some(&'=') | Some(&'!')) => {
            Some("a group named `(?<name>...)`, which Python spells `(?P<name>...)`")
        }
        _ => inline_flag_group(characters),
    }
}

/// The inline flag groups Python and the engines disagree about.
///
/// `(?a)` is the ASCII flag. It narrows `\w`, `\W`, `\d`, `\D`, `\s`, `\S`,
/// `\b` and `\B` to ASCII, and — with `i` — narrows case folding too, so
/// `(?ai)é` does not match `É`. Neither engine has it: the linear one knows
/// `(?-u:...)`, which narrows the shorthands but also makes `.` match any byte,
/// and the fallback engine refuses to change Unicode mode inline at all. A
/// translation of the eight shorthands alone would leave `\b` and the case
/// folding reading text Python would not match, and a wrong match is worse than
/// an error, so the flag is refused rather than half-translated.
///
/// Python refuses a flag *turned off* outside a group the same way — it wants
/// `(?-i:...)`, and it wants the `:` — and the engines accept `(?-i)`, so the
/// sign counts as a refusal here too.
fn inline_flag_group(characters: &[char]) -> Option<&'static str> {
    let mut length = 0;
    let mut turned_off = false;
    while let Some(flag) = characters.get(2 + length) {
        match *flag {
            '-' => turned_off = true,
            'a' | 'i' | 'L' | 'm' | 's' | 'u' | 'x' => {}
            _ => break,
        }
        length += 1;
    }
    let flags = &characters[2..2 + length];
    let scoped = characters.get(2 + length) == Some(&':');
    // Anything else opening with `(?` is either not a flag group, or one the
    // engines do not take either and their message says more about it. `(?L)`
    // and `(?x)` land here and are refused by both engines already.
    if flags.is_empty() || (!scoped && characters.get(2 + length) != Some(&')')) {
        return None;
    }
    if flags.contains(&'a') {
        Some("the ASCII flag, `(?a)`, which neither engine here can express")
    } else if turned_off && !scoped {
        Some("an inline flag turned off outside a group, which Python spells `(?-i:...)`")
    } else {
        None
    }
}

/// What a backslash before a digit is.
enum NumberedEscape {
    /// A character: the text to write for it and how many digits it spans.
    Octal { text: String, span: usize },
    /// A back-reference, which both engines already read the way Python does.
    BackReference,
}

/// Read the digits of a numeric escape, which is where Python and the engines
/// part ways: Python reads `\012` as the newline and `\123` as `S`, and both
/// engines read them as a back-reference to group 12 and group 123.
///
/// The rule is `sre_parse`'s, and it is three cases rather than a grammar: a
/// leading `0` takes at most two more octal digits; three octal digits are a
/// character whatever the leading digit is; anything else is a back-reference.
/// That is why `\0377` is `\x1f` and a literal `7`, and why `\128` is a
/// back-reference to group 12 and a literal `8` — the third digit has to be
/// octal for the three-digit reading, and `8` is not.
///
/// Inside a class every numeric escape is a character, which is why `[\1]` is
/// `\x01` and `[\19]` is `\x01` and a literal `9`; Python reads `[\12]` as the
/// newline, which is exactly what this translation has to reproduce.
///
/// A value above `\377` is refused rather than truncated, as Python refuses it:
/// `\400` is an error there, and the fallback engine would otherwise have read
/// it as a back-reference to group 400. `[\8]` is left alone, because Python
/// refuses that escape too and both engines refuse the class it appears in.
///
/// One divergence is left open: with twelve groups, `\128` is group 12 and a
/// literal `8` in Python and group 128 — which does not exist — in the engines,
/// so the pattern is refused here rather than run. The answer is wrong only in
/// being louder than QD's.
fn python_numbered_escape(
    digits: &[char],
    in_class: bool,
) -> std::result::Result<NumberedEscape, &'static str> {
    let Some(&first) = digits.first() else {
        return Ok(NumberedEscape::BackReference);
    };
    let octal_digits = digits
        .iter()
        .take_while(|digit| ('0'..='7').contains(*digit))
        .count();
    let span = if first == '0' {
        // The zero, and at most two more.
        1 + (octal_digits - 1).min(2)
    } else if in_class {
        match octal_digits {
            0 => return Ok(NumberedEscape::BackReference),
            count => count.min(3),
        }
    } else if octal_digits >= 3 {
        3
    } else {
        return Ok(NumberedEscape::BackReference);
    };
    let value = digits[..span].iter().fold(0u32, |value, digit| {
        value * 8 + (*digit as u32 - '0' as u32)
    });
    if value > 0o377 {
        return Err("an octal escape above `\\377`, which Python refuses");
    }
    Ok(NumberedEscape::Octal {
        text: format!("\\x{value:02x}"),
        span,
    })
}

/// A repetition as Python's `re` reads one.
struct PythonRepetition {
    /// How many characters of the pattern it spans, braces included.
    length: usize,
    /// `{,n}` and `{,}` omit the minimum, which Rust's engine has no shorthand
    /// for: `{,2}` is `{0,2}` and `{,}` is `{0,}`.
    omitted_minimum: bool,
    /// `{m,n}` with `m > n`. Python raises `min repeat greater than max
    /// repeat`, and Rust's linear engine refuses it too — but the backtracking
    /// engine reads it as a match, so it has to be caught before either is
    /// asked.
    minimum_exceeds_maximum: bool,
}

/// The repetition at the start of `characters` (which begins with `{`).
///
/// `{m}`, `{m,}`, `{m,n}` and their Python shorthand `{,n}` and `{,}` come back
/// as repetitions; anything else — `{}`, `{ 2 }`, `{a,b}`, `{2,3,4}`, a lone
/// `{` — is not one at all, which is what makes the brace literal.
fn python_repetition(characters: &[char]) -> Option<PythonRepetition> {
    let mut index = 1;
    let minimum_start = index;
    while characters.get(index).is_some_and(char::is_ascii_digit) {
        index += 1;
    }
    let minimum = &characters[minimum_start..index];
    let omitted_minimum = minimum.is_empty();
    let mut comma = false;
    let mut maximum: &[char] = &[];
    if characters.get(index) == Some(&',') {
        comma = true;
        index += 1;
        let maximum_start = index;
        while characters.get(index).is_some_and(char::is_ascii_digit) {
            index += 1;
        }
        maximum = &characters[maximum_start..index];
    }
    // `{}` has neither a minimum nor a comma, so it is a literal brace.
    if characters.get(index) != Some(&'}') || (omitted_minimum && !comma) {
        return None;
    }
    Some(PythonRepetition {
        length: index + 1,
        omitted_minimum,
        minimum_exceeds_maximum: !omitted_minimum
            && !maximum.is_empty()
            && exceeds(minimum, maximum),
    })
}

/// Whether the minimum of a repetition is greater than its maximum.
///
/// The digits are compared as digits rather than parsed into a number, so that
/// a repetition too large for a `u64` is judged by the same rule Python applies
/// instead of overflowing.
fn exceeds(minimum: &[char], maximum: &[char]) -> bool {
    let significant = |digits: &[char]| {
        let start = digits
            .iter()
            .position(|digit| *digit != '0')
            .unwrap_or(digits.len());
        digits[start..].to_vec()
    };
    let (minimum, maximum) = (significant(minimum), significant(maximum));
    minimum.len() > maximum.len() || (minimum.len() == maximum.len() && minimum > maximum)
}

fn compile_regex(pattern: &str) -> Result<CompiledRegex> {
    let (body, flags) = qd_pattern(pattern);
    flags
        .build(pattern, body)
        .map_err(|reason| anyhow::anyhow!("invalid QD regular expression {pattern:?}: {reason}"))
}

/// Whether Python reads `\` before this character as the character itself.
///
/// Python lets a backslash escape any character that is not a letter or a
/// digit: `\<` is `<` and `\:` is `:`. Rust's engine takes a backslash before a
/// letter (`\d`, `\n`) and before its own metacharacters (`\.`, `\*`), so what
/// is left is punctuation that means itself anyway — and a backslash in front
/// of it is refused there.
///
/// Dropping the backslash is exactly Python's reading, and it also takes those
/// characters away from the fallback engine, whose oniguruma vocabulary gives
/// `\<` and `\>` a meaning of their own. yunyaokz.har is written that way:
/// `(?<=累计已签到:  \<b\>)` has to look behind `<b>`, which it would silently
/// stop doing if `\<` were left as a word boundary.
fn means_itself(escaped: char) -> bool {
    escaped.is_ascii_punctuation() && !RUST_METACHARACTERS.contains(escaped)
}

/// The characters Rust's engine lets a backslash escape, which is the set it
/// gives a meaning of its own.
const RUST_METACHARACTERS: &str = r"\.+*?()|[]{}^$#&-~";

/// The ASCII letters Python gives a meaning to after a backslash.
///
/// `\Z` and `\N` are on it and are handled above; everything else is an
/// ordinary escape both engines read the same way. Any *other* letter is a
/// `bad escape` in Python, so refusing it keeps the fallback engine's
/// extra vocabulary — `\h` horizontal space, `\R` a line break, `\X` a
/// grapheme, `\p{...}` a Unicode property — from making patterns legal here
/// that QD would refuse.
fn knows_escape(escaped: char) -> bool {
    !escaped.is_ascii_alphabetic() || "abfnrtvdDsSwWABZNxuU".contains(escaped)
}

/// The text to compile and the flags to compile it with. A pattern that is not
/// delimited carries none.
fn qd_pattern(pattern: &str) -> (&str, QdFlags) {
    split_qd_regex(pattern).unwrap_or((pattern, QdFlags::default()))
}

/// The delimited form, parsed the way qd-today parses it — `^/(.*?)/([gimsu]*)$`,
/// which is the last `/` whose tail is all flag letters.
///
/// The quirks are QD's, and are kept on purpose: the flag set is exactly
/// `gimsu`, so `/(\d+)/x` is not a delimited regex at all but the literal
/// pattern `/(\d+)/x`, and a string that does not parse is never an error, it
/// is simply a pattern with no flags.
fn split_qd_regex(pattern: &str) -> Option<(&str, QdFlags)> {
    let rest = pattern.strip_prefix('/')?;
    let slash = rest.rfind('/')?;
    let (body, suffix) = rest.split_at(slash);
    let flags = &suffix[1..];
    flags
        .chars()
        .all(|flag| "gimsu".contains(flag))
        .then(|| (body, QdFlags::parse(flags)))
}

fn extract(pattern: &str, source: &str) -> Result<Option<Value>> {
    let regex = compile_regex(pattern)?;
    let (_, flags) = qd_pattern(pattern);
    if flags.global {
        // Python's `re.findall`: every match at once, each shaped by the
        // pattern's group count.
        return Ok(Some(Value::Array(regex.find_all(source)?)));
    }
    regex.first(source)
}

/// Python's `re.findall(pattern, source, re.IGNORECASE)`, shaped the way
/// `re.findall` shapes its entries.
///
/// This is what `api://util/regex` computes (qd's `UtilRegexHandler` calls
/// `re.findall(p, data, re.IGNORECASE)` on the pattern as written — no
/// `/…/flags` delimiters, which that handler never parses), and it borrows the
/// same dialect translation every other QD pattern gets.
pub(crate) fn python_findall(pattern: &str, source: &str) -> Result<Vec<Value>> {
    let flags = QdFlags {
        case_insensitive: true,
        ..QdFlags::default()
    };
    flags
        .build(pattern, pattern)
        .map_err(|reason| anyhow::anyhow!("invalid QD regular expression {pattern:?}: {reason}"))?
        .find_all(source)
}

/// One entry of a `findall` result. Python decides its shape by how many
/// groups the pattern has, and templates index into it accordingly.
fn qd_findall_entry<C: QdCaptures + ?Sized>(capture: &C) -> Value {
    let group = |index: usize| {
        Value::String(
            capture
                .group(index)
                // A group that took no part in the match is "" in Python too.
                .unwrap_or_default()
                .to_string(),
        )
    };
    match capture.group_count() {
        // No groups: the whole match.
        1 => group(0),
        // One group: that group, not the whole match.
        2 => group(1),
        // Several: one tuple per match, which templates index as `item[0]`.
        groups => Value::Array((1..groups).map(group).collect()),
    }
}

/// What QD stores for a non-global `extract_variables`: `re.search`, then
/// `m.groups()[0]` when the pattern has groups and `m.group(0)` when it has
/// none — that is, the first group or the whole match.
fn qd_first_value<C: QdCaptures + ?Sized>(capture: &C) -> Option<Value> {
    let matched = if capture.group_count() > 1 {
        capture.group(1)
    } else {
        capture.group(0)
    };
    // QD stores Python's `None` when the first group took no part, and that
    // reaches the template as the text "None". The whole match is the only
    // other thing it could be, and the more useful of the two.
    let matched = matched.or_else(|| capture.group(0))?;
    Some(Value::String(matched.into()))
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

/// Build the client one outbound request uses, pinned to the addresses `url`
/// just resolved to.
///
/// This is the only place the SSRF policy becomes a client. The executor and
/// every server-side fetch (task targets, notification channels, library
/// sources) go through it, so hardening cannot land on one caller and miss
/// another, and a new relaxation is added to [`OutboundPolicy`] where both
/// sides have to see it rather than to one caller's builder chain.
///
/// The client is per request, not per process: pinning is a client-wide
/// `host -> address` mapping, so a shared client could only ever pin one
/// host. The cost is a connection pool per request, which the executor has
/// always paid.
pub async fn guarded_client_for_url(
    url: &str,
    policy: &OutboundPolicy,
    cookies: Option<Arc<Jar>>,
) -> Result<Client> {
    let (parsed, addresses) = resolve_target(url, policy.allow_private_network).await?;
    let host = parsed.host_str().context("URL host is missing")?;
    let mut builder = Client::builder()
        .redirect(Policy::none())
        .danger_accept_invalid_certs(policy.allow_invalid_certificates)
        .timeout(policy.timeout);
    if let Some(cookies) = cookies {
        builder = builder.cookie_provider(cookies);
    }
    if let Some(proxy) = policy.proxy.as_deref() {
        // With a proxy, DNS is delegated to the proxy; do not pin the host. The
        // proxy address itself still has to pass the same classification: a
        // user-supplied `_proxy` pointing at loopback/private/metadata would
        // otherwise make *it* the connection and bypass the SSRF switch.
        let proxy_url = classify_proxy(proxy, policy.allow_private_network).await?;
        return builder
            .proxy(reqwest::Proxy::all(proxy_url.as_str()).context("invalid proxy URL")?)
            .build()
            .context("cannot build proxied HTTP client");
    }
    // Pin every address the guard classified, not just the first. A dual-stack
    // name (an intranet dual-stack DNS is how this surfaced) resolves to both
    // families, and getaddrinfo happily puts the dead one first — pinning only
    // that made every connect fail with "connection refused" even though a
    // later address was reachable, where curl, which walks the list, connected.
    // Handing hyper the whole list keeps the guard's guarantee — the client can
    // only dial addresses that were classified — while regaining the fallback.
    builder
        .resolve_to_addrs(host, &addresses)
        .build()
        .context("cannot build pinned HTTP client")
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

/// Parse a proxy URL and classify its host with the same rules as a target.
///
/// HTTP(S) and SOCKS5 are accepted (QD's `_proxy` supports all three). The host
/// is resolved and, unless the admin allowed private networks, every address it
/// resolves to must be public — otherwise a template's `_proxy` would be a way
/// around the SSRF switch. Returns the parsed URL to hand to reqwest.
async fn classify_proxy(proxy: &str, allow_private_network: bool) -> Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(proxy).context("invalid proxy URL")?;
    ensure!(
        matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h"),
        "unsupported proxy scheme"
    );
    let host = parsed.host_str().context("proxy host is missing")?;
    let port = parsed
        .port_or_known_default()
        .context("proxy port is missing")?;
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .context("cannot resolve proxy host")?
        .collect::<Vec<_>>();
    ensure!(!addresses.is_empty(), "proxy host resolved to no addresses");
    if !allow_private_network {
        ensure!(
            addresses.iter().all(|address| is_public_ip(address.ip())),
            "private or special-use network target is blocked"
        );
    }
    Ok(parsed)
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

    use crate::{
        plugin::{PLUGIN_API_VERSION, PluginManifest, PluginRequest, PluginResponse},
        qd_har::{QdHar, QdProgram},
    };

    fn local_executor() -> QdExecutor {
        QdExecutor::with_options(ExecutorOptions {
            timeout: Duration::from_secs(5),
            allow_private_network: true,
            ..ExecutorOptions::default()
        })
        .unwrap()
    }

    /// The pieces of the encoding choice nothing else pins down: how a
    /// `charset` parameter is read off a header, the three declarations a body
    /// can carry, QD's own labels, and which source wins when two speak.
    #[test]
    fn the_encoding_sources_follow_qd() {
        // The `charset` parameter of a `Content-Type`, quoted or bare.
        assert_eq!(
            charset_parameter("text/html; charset=gb2312"),
            Some("gb2312")
        );
        assert_eq!(charset_parameter("text/html; charset=\"GBK\""), Some("GBK"));
        assert_eq!(charset_parameter("text/html"), None);

        // The three declarations a body can carry, and the ASCII-ness they
        // rely on being readable in raw bytes.
        assert_eq!(
            declared_in_body(b"<meta charset=\"gb2312\">"),
            Some("gb2312".to_string())
        );
        assert_eq!(
            declared_in_body(b"<meta content=\";charset=big5\">"),
            Some("big5".to_string())
        );
        assert_eq!(
            declared_in_body(b"<?xml version=\"1.0\" encoding=\"utf-8\"?>"),
            Some("utf-8".to_string())
        );

        // QD's own labels: `unicode` is UTF-8, `gb2312` is read as `gb18030`
        // (a site says the narrow set, its bytes use the wide one), and
        // `latin_1` is Windows-1252.
        assert_eq!(encoding_for("unicode").unwrap().name(), "UTF-8");
        assert_eq!(encoding_for("gb2312").unwrap().name(), "gb18030");
        assert_eq!(encoding_for("latin_1").unwrap().name(), "windows-1252");

        // A header and a body can both speak; the header is the response's own
        // word, so it wins.
        assert_eq!(
            find_encoding(
                b"<meta charset=\"big5\">",
                &[("content-type".into(), "text/html; charset=utf-8".into())],
            )
            .name(),
            "UTF-8"
        );
        // `ISO-8859-1` is what a stack writes when nobody decided, so it is
        // discarded and the body is read by its own declaration.
        assert_eq!(
            find_encoding(
                b"<meta charset=\"gb2312\">",
                &[(
                    "content-type".into(),
                    "text/html; charset=ISO-8859-1".into()
                )],
            )
            .name(),
            "gb18030"
        );
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
    async fn ignores_http2_pseudo_headers_from_browser_captures() {
        // Issue #6: QD's Chrome export keeps the HTTP/2 pseudo-headers
        // `:method`/`:path`/`:scheme` in `request.headers`. reqwest rejects
        // those as illegal header names, so the executor has to drop them: the
        // method and URL already carry the same information.
        let app = Router::new().route("/user.php", get(|| async { "ok" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let har = QdHar::parse(json!({"log": {"version": "1.2", "entries": [{
            "checked": true,
            "request": {
                "method": "GET",
                "url": format!("http://{address}/user.php?id=62692"),
                "headers": [
                    {"name": ":method", "value": "GET", "checked": true},
                    {"name": ":path", "value": "/user.php?id=62692", "checked": true},
                    {"name": ":scheme", "value": "https", "checked": true},
                    {"name": "user-agent", "value": "qd", "checked": true}
                ],
                "cookies": []
            },
            "success_asserts": [{"re": "200", "from": "status"}]
        }]}}))
        .unwrap();
        let program = QdProgram::compile(&har).unwrap();
        let executor = local_executor();
        let mut context = ExecutionContext::new(BTreeMap::new());

        let results = executor.execute(&program, &mut context).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, 200);
        assert_eq!(results[0].body_size, 2);
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

    /// The shared constructor is where the guard becomes a client, so the two
    /// switches have to be readable from it — not only from an executor built
    /// with the same options.
    #[tokio::test]
    async fn the_shared_constructor_applies_the_same_guard() {
        let url = format!("http://{}", loopback_address());
        let blocked = guarded_client_for_url(&url, &OutboundPolicy::default(), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            blocked.contains("private or special-use network target is blocked"),
            "a default policy must refuse a loopback target: {blocked}"
        );
        guarded_client_for_url(
            &url,
            &OutboundPolicy {
                allow_private_network: true,
                ..OutboundPolicy::default()
            },
            None,
        )
        .await
        .expect("the relaxation must let the same URL through");
    }

    /// A multi-address name can resolve its dead address first. This surfaced
    /// on an intranet dual-stack DNS (#27): `flaresolverr.d.test` answered with
    /// an unreachable v6 ULA ahead of a working v4 address, and pinning only
    /// the first classified address turned every connect into "connection
    /// refused" where curl, which walks the list, connected. The pinned client
    /// must carry every address the guard classified so the connector can fall
    /// back. `localhost` is the one portable dual-family name: with the server
    /// on v4 only, this connects whatever order the resolver returns — and it
    /// fails again if pinning ever shrinks back to one address on a platform
    /// that lists the v6 one first.
    #[tokio::test]
    async fn a_multi_address_host_connects_when_its_first_address_is_dead() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, Router::new()).await.unwrap() });

        let client = guarded_client_for_url(
            &format!("http://localhost:{port}/"),
            &OutboundPolicy {
                allow_private_network: true,
                ..OutboundPolicy::default()
            },
            None,
        )
        .await
        .unwrap();

        // An empty router answers 404 for everything; reaching it at all is
        // the assertion.
        let status = client
            .get(format!("http://localhost:{port}/"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    }

    /// A template's `_proxy` is user input, so the proxy host must pass the same
    /// classification as the target. Without this, `_proxy=http://169.254.169.254`
    /// would make the proxy itself the connection and walk around the switch.
    #[tokio::test]
    async fn a_proxy_host_is_classified_like_a_target() {
        let loopback = format!("http://{}", loopback_address());
        let error = classify_proxy(&loopback, false)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("private or special-use network target is blocked"),
            "a loopback proxy must be refused: {error}"
        );
        classify_proxy(&loopback, true)
            .await
            .expect("the relaxation must allow an internal proxy");
        // Schemes and shapes that cannot be proxied through are refused rather
        // than silently downgraded.
        assert!(classify_proxy("ftp://example.com:21", true).await.is_err());
        assert!(classify_proxy("socks5://127.0.0.1", true).await.is_err());
    }

    #[tokio::test]
    async fn the_run_policy_takes_its_proxy_from_the_proxy_variable() {
        let executor = QdExecutor::new(Duration::from_secs(5)).unwrap();
        let mut context = ExecutionContext::new(BTreeMap::new());
        assert!(executor.policy_with_proxy(&context).proxy.is_none());

        context
            .variables
            .insert("_proxy".into(), json!("http://proxy.test:8080"));
        assert_eq!(
            executor.policy_with_proxy(&context).proxy.as_deref(),
            Some("http://proxy.test:8080")
        );

        // Empty and whitespace both mean "no proxy" — the `default("")` case —
        // and must also clear a proxy the run policy started with.
        context.variables.insert("_proxy".into(), json!("   "));
        let executor = QdExecutor::with_options(ExecutorOptions {
            proxy: Some("http://default.test:8080".into()),
            ..ExecutorOptions::default()
        })
        .unwrap();
        assert!(executor.policy_with_proxy(&context).proxy.is_none());
    }

    /// The executor must reach a client only through the shared constructor: a
    /// second builder chain is how a hardening lands on the server fetches and
    /// misses template runs, or the reverse. Structural because the
    /// alternative — reaching a private address from a template — is exactly
    /// what the guard prevents.
    ///
    /// Formatting-independent on purpose. An earlier version of this test
    /// matched the call site's line breaks and a refactor moved the call onto
    /// one line, which would have left it green on a file it no longer
    /// examined; a needle that can be reflowed out from under the assertion is
    /// not an assertion. This reads the production half of the file — cut at
    /// the first `#[cfg(test)]`, which is the pinned-client helper used by the
    /// tests below — and checks the two facts that matter: the executor calls
    /// the shared constructor, and it does not build a client itself.
    ///
    /// The needle carries no `reqwest::` prefix, because this file imports the
    /// type: a builder chain written here reads `Client::builder()`, and a
    /// prefix-anchored needle would wave exactly that through.
    #[test]
    fn the_executor_reaches_a_client_only_through_the_shared_constructor() {
        let source = include_str!("executor.rs").replace("\r\n", "\n");
        // Cut at the test module rather than at the first `#[cfg(test)]`. The
        // pinned-client helper carries that attribute from the middle of the
        // file, so the first-marker rule ended this half at the helper and left
        // everything below it — the shared constructor and every free function
        // after it — unexamined.
        let production = source
            .split("\nmod tests {")
            .next()
            .expect("the file is never empty");
        assert!(
            production.contains("guarded_client_for_url("),
            "the executor no longer calls the shared constructor"
        );
        // Exactly two builder chains belong in this half, and naming both is
        // what lets a third one fail with a message that says which two were
        // expected: the shared constructor, which *is* the gate and has to
        // build a client, and the `#[cfg(test)]` helper that proves address
        // pinning works. "No builder at all" cannot be true here, and checking
        // only the full `reqwest::Client` path would miss a `Client::new()`
        // written against the imported type.
        let builder = ["Client::", "builder()"].concat();
        assert_eq!(
            production.matches(&builder).count(),
            2,
            "a client is being built outside guarded_client_for_url and the pinned helper"
        );
        assert!(
            !production.contains("Client::new()"),
            "the executor builds its own client; every request goes through guarded_client_for_url"
        );
    }

    /// The plugin's own test proves the guard works once a policy reaches it.
    /// This proves a policy reaches it. The executor could keep a correct
    /// `UtilityPlugin` wired to a closed default while opening its own posture,
    /// and the DdddOCR forward would be unguarded again — which is the shape of
    /// the hole this replaced, so the wiring gets its own assertion rather than
    /// resting on the plugin's.
    #[test]
    fn the_executor_hands_its_policy_to_the_builtin_plugin() {
        let source = include_str!("executor.rs").replace("\r\n", "\n");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("the file is never empty");
        assert!(
            production.contains("UtilityPlugin::with_policy("),
            "the built-in plugin is registered without this run's policy"
        );
        assert!(
            !production.contains("UtilityPlugin::default()"),
            "the built-in plugin falls back to a closed default, so its DdddOCR \
             forward ignores the switches"
        );
    }

    /// A bound-but-unconnected loopback port: enough for the guard, which
    /// classifies the address before anything is sent.
    fn loopback_address() -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
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
            "extract_variables": [{"name": "delay_result", "re": "(delay .+)", "from": "content"}]
        }]}}))
        .unwrap();
        let program = QdProgram::compile(&har).unwrap();
        let executor = QdExecutor::new(Duration::from_secs(1)).unwrap();
        let mut context = ExecutionContext::new(BTreeMap::new());

        let results = executor.execute(&program, &mut context).await.unwrap();

        assert_eq!(results[0].status, 200);
        assert_eq!(
            context.variables.get("delay_result"),
            Some(&json!("delay 0.0 second."))
        );
    }

    /// Minimal in-process plugin: proves the executor wires custom plugins
    /// end to end (request -> assertions -> extract_variables) without having
    /// to spawn a subprocess in a unit test.
    struct MockPlugin {
        manifest: PluginManifest,
    }

    impl Default for MockPlugin {
        fn default() -> Self {
            Self {
                manifest: PluginManifest {
                    api_version: PLUGIN_API_VERSION,
                    id: "mock".into(),
                    name: "Mock echo".into(),
                    version: "1.0.0".into(),
                    capabilities: Vec::new(),
                },
            }
        }
    }

    impl Plugin for MockPlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }

        fn call<'a>(
            &'a self,
            request: &'a PluginRequest,
        ) -> Pin<Box<dyn Future<Output = Result<PluginResponse>> + Send + 'a>> {
            Box::pin(async move {
                let text = request.query.get("text").map(String::as_str).unwrap_or("");
                Ok(PluginResponse {
                    status: 200,
                    headers: BTreeMap::new(),
                    body: format!("echo:{text}").into_bytes(),
                })
            })
        }
    }

    fn mock_echo_har() -> QdHar {
        QdHar::parse(json!({"log": {"version": "1.2", "entries": [{
            "checked": true,
            "request": {"method": "GET", "url": "api://mock/echo?text=hello"},
            "success_asserts": [{"re": "200", "from": "status"}],
            "extract_variables": [{"name": "echoed", "re": "echo:(.+)", "from": "content"}]
        }]}}))
        .unwrap()
    }

    #[tokio::test]
    async fn registered_plugin_body_feeds_extract_variables() {
        let mut executor = local_executor();
        executor
            .register_plugin(Arc::new(MockPlugin::default()))
            .unwrap();
        assert!(executor.plugin_ids().contains(&"mock".to_string()));
        let program = QdProgram::compile(&mock_echo_har()).unwrap();
        let mut context = ExecutionContext::new(BTreeMap::new());

        let results = executor.execute(&program, &mut context).await.unwrap();

        assert_eq!(results[0].status, 200);
        assert_eq!(context.variables.get("echoed"), Some(&json!("hello")));
    }

    #[tokio::test]
    async fn without_registration_the_same_failure_stays_diagnosable() {
        // Regression guard: an empty registry behaves exactly as before, only
        // the message now names the plugin, the action and what is registered.
        let executor = local_executor();
        let program = QdProgram::compile(&mock_echo_har()).unwrap();
        let mut context = ExecutionContext::new(BTreeMap::new());

        let error = executor
            .execute(&program, &mut context)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("plugin unavailable: mock/echo"), "{error}");
        assert!(error.contains("registered: util"), "{error}");

        // The built-in plugin is untouched by the new registration path.
        let delay = QdHar::parse(json!({"log": {"version": "1.2", "entries": [{
            "checked": true,
            "request": {"method": "GET", "url": "api://util/delay?seconds=0"},
            "success_asserts": [{"re": "200", "from": "status"}]
        }]}}))
        .unwrap();
        let results = executor
            .execute(&QdProgram::compile(&delay).unwrap(), &mut context)
            .await
            .unwrap();
        assert_eq!(results[0].status, 200);
    }

    #[test]
    fn refuses_to_register_a_duplicate_plugin_id() {
        let mut executor = local_executor();
        let error = executor
            .register_plugin(Arc::new(UtilityPlugin::default()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("already registered"), "{error}");
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

    /// Issue #26: a delimited pattern used to be compiled with
    /// `.unicode(false)`, and a `.` under that flag can match a byte, which a
    /// `str` pattern may not — so every `/(…)/` regex containing `.` failed to
    /// compile ("pattern can match invalid UTF-8") before it ever ran.
    #[test]
    fn a_delimited_pattern_keeps_python_unicode_semantics() {
        // pcbeta.har, verbatim.
        assert_eq!(
            extract(r#"/"name": "(.+?)"/g"#, r#"{"name": "张三"}"#).unwrap(),
            Some(json!(["张三"]))
        );
        // `.` reaches a Chinese character…
        assert_eq!(extract("/(.+)/g", "中文").unwrap(), Some(json!(["中文"])));
        // …and `\w` is Unicode word characters, the default in both engines,
        // whether or not the pattern asks for `u`.
        assert!(compile_regex("/\\w+/").unwrap().is_match("中文").unwrap());
        assert!(compile_regex("/\\w+/u").unwrap().is_match("中文").unwrap());
    }

    /// The five letters QD accepts, mapped to the matches Python makes.
    #[test]
    fn qd_flags_map_to_the_same_matches_python_makes() {
        assert!(
            compile_regex("/^hello/im")
                .unwrap()
                .is_match("x\nHello")
                .unwrap()
        );
        // `s` is what lets `.` cross a newline; without it, it does not.
        assert!(compile_regex("/a.b/s").unwrap().is_match("a\nb").unwrap());
        assert!(!compile_regex("/a.b/").unwrap().is_match("a\nb").unwrap());
        // `g` is QD's own flag rather than the engine's — it picks findall over
        // search, and it is a set, not a sequence.
        assert_eq!(extract("/a/", "a a").unwrap(), Some(json!("a")));
        assert_eq!(extract("/a/g", "a a").unwrap(), Some(json!(["a", "a"])));
    }

    /// `split_qd_regex` mirrors QD's `^/(.*?)/([gimsu]*)$`, so the strings QD
    /// does *not* read as delimited have to stay undelimited here too.
    #[test]
    fn qd_delimiters_are_read_the_way_qd_reads_them() {
        // `x` is not one of QD's flags: this is the literal pattern `/a/x`.
        assert!(compile_regex("/a/x").unwrap().is_match("/a/x").unwrap());
        assert!(!compile_regex("/a/x").unwrap().is_match("a").unwrap());
        // Neither is a trailing path segment, however flag-like it looks.
        assert!(
            compile_regex("/api/v1/x")
                .unwrap()
                .is_match("/api/v1/x")
                .unwrap()
        );
        // A body may hold slashes; the split is at the last one that fits.
        assert_eq!(
            extract(r#"/<a href="/album/(\d+)//g"#, r#"<a href="/album/42/"#).unwrap(),
            Some(json!(["42"]))
        );
    }

    /// Python's `findall` shapes each entry by the pattern's group count: the
    /// whole match with none, the group with one, a tuple with several — and
    /// an empty string for a group that took no part.
    #[test]
    fn findall_shapes_follow_the_group_count() {
        assert_eq!(
            extract("/\\d+/g", "a1b22c").unwrap(),
            Some(json!(["1", "22"]))
        );
        assert_eq!(
            extract("/\\d(\\d)/g", "a12b34").unwrap(),
            Some(json!(["2", "4"]))
        );
        assert_eq!(
            extract("/(\\d)(\\d)/g", "a12b34").unwrap(),
            Some(json!([["1", "2"], ["3", "4"]]))
        );
        assert_eq!(
            extract("/(a)|(b)/g", "ab").unwrap(),
            Some(json!([["a", ""], ["", "b"]]))
        );
        // No match at all is an empty list in Python, not `None`.
        assert_eq!(extract("/\\d+/g", "abc").unwrap(), Some(json!([])));
    }

    #[test]
    fn a_non_global_extraction_is_the_first_group_or_the_whole_match() {
        assert_eq!(extract("/\\d+/", "a12b").unwrap(), Some(json!("12")));
        assert_eq!(extract("/\\d(\\d)/", "a12b").unwrap(), Some(json!("2")));
        assert_eq!(extract("/\\d+/", "abc").unwrap(), None);
        // QD stores Python's `None` when the first group took no part, which a
        // template renders as the text "None"; the whole match is the other
        // candidate and the more useful one.
        assert_eq!(extract("/(?:x(\\d))|y/", "y").unwrap(), Some(json!("y")));
    }

    /// Lookaround and back-references are Python constructs Rust's linear
    /// engine does not have, and 34 regexes across 10 of the 387 templates in
    /// qd-today/templates use them. They are what the backtracking fallback is
    /// for, so what matters is not that they compile but that they *match* what
    /// Python matches.
    #[test]
    fn lookaround_matches_what_python_matches() {
        // Positive look-behind and look-ahead, 2Libra每日签到.har and
        // NS云社区.har verbatim.
        assert_eq!(
            extract(r#"(?<="coins":)\d+"#, r#"{"coins":7,"name":"a"}"#).unwrap(),
            Some(json!("7"))
        );
        assert_eq!(
            extract(r#""name":"([^"]*)"(?=,)"#, r#""name":"x","a":1"#).unwrap(),
            Some(json!("x"))
        );
        // A look-behind is zero-width: it is not part of what is extracted.
        assert_eq!(
            extract(r#"(?<="m":").*?(?=")"#, r#"{"m":"hello"}"#).unwrap(),
            Some(json!("hello"))
        );
        // A negative look-ahead, Linux_SB.har: everything that is not a bare
        // `200`.
        assert!(
            compile_regex(r#"^(?!200$)"#)
                .unwrap()
                .is_match("404")
                .unwrap()
        );
        assert!(
            !compile_regex(r#"^(?!200$)"#)
                .unwrap()
                .is_match("200")
                .unwrap()
        );
        // Flags reach the fallback too, and `g` still selects findall — the
        // pattern has to be delimited for QD to read `g` at all.
        assert_eq!(
            extract(r#"/(?<=":)\d+(?=,)/g"#, r#"{"a":1,"b":22,"c":3}"#).unwrap(),
            Some(json!(["1", "22"]))
        );
        // A look-behind and a look-ahead together, 搜书吧.har verbatim, in its
        // delimited and global form.
        assert_eq!(
            extract(
                r#"/最新主题[\s\S]+?tid=(\d+)[\s\S]+?tid=(\d+)[\s\S]+?tid=(\d+)[\s\S]+?(?<=valign="top")/g"#,
                r#"最新主题 x tid=1 y tid=2 z tid=3 valign="top""#
            )
            .unwrap(),
            Some(json!([["1", "2", "3"]]))
        );
        // A look-behind needs fixed width in Python, and `\s` is one character;
        // 精睿论坛.har and OpenFRP.har use this shape.
        assert_eq!(
            extract(
                r#"(?<=Set-Cookie: 17a=)[^;]+"#,
                "Set-Cookie: 17a=abc123; Path=/"
            )
            .unwrap(),
            Some(json!("abc123"))
        );
        // A back-reference. The extraction is the first group, so `bb` — the
        // whole match — only reaches the template through `is_match`.
        assert_eq!(extract(r#"(\w)\1"#, "abbc").unwrap(), Some(json!("b")));
        assert!(compile_regex(r#"(\w)\1"#).unwrap().is_match("aa").unwrap());
        assert!(!compile_regex(r#"(\w)\1"#).unwrap().is_match("ab").unwrap());
        // The flags reach the fallback engine the same way they reach the
        // linear one.
        assert!(
            compile_regex(r"/(?<=x)y/i")
                .unwrap()
                .is_match("XY")
                .unwrap()
        );
        assert!(!compile_regex(r"/(?<=x)y/").unwrap().is_match("XY").unwrap());
    }

    /// The linear engine is tried first and the second one only when it must
    /// be, so the engine that can be made slow is reached by 34 patterns out of
    /// 6182 and not by the other 6148.
    #[test]
    fn only_a_pattern_the_linear_engine_refuses_reaches_the_fallback() {
        assert!(matches!(
            compile_regex(r#"/"name": "(.+?)"/g"#).unwrap(),
            CompiledRegex::Linear(_)
        ));
        // No lookaround, but `regex` will not read Python's literal brace —
        // that is a translation, so the linear engine still takes it.
        assert!(matches!(
            compile_regex(r#"{"total":(\d+)"#).unwrap(),
            CompiledRegex::Linear(_)
        ));
        // `{,n}` and `{,}` are the same story: `regex` refuses them, the
        // translation writes the zero out, and the linear engine takes the
        // result. The reading is identical either way, because the fallback
        // engine reads `{,n}` the way Python does — what the translation buys
        // is the engine, and that is what these assert.
        assert!(matches!(
            compile_regex(r"a{,2}").unwrap(),
            CompiledRegex::Linear(_)
        ));
        assert!(matches!(
            compile_regex(r"a{,}").unwrap(),
            CompiledRegex::Linear(_)
        ));
        // Python's literal `[` in a class is another: `[[:alpha:]]` is not a
        // POSIX class in Python, and escaping the bracket keeps it away from
        // the fallback engine's reading of one.
        assert!(matches!(
            compile_regex(r"[[:alpha:]]").unwrap(),
            CompiledRegex::Linear(_)
        ));
        assert!(matches!(
            compile_regex(r#"(?<="coins":)\d+"#).unwrap(),
            CompiledRegex::Backtracking { .. }
        ));
        assert!(matches!(
            compile_regex(r#"(\w)\1"#).unwrap(),
            CompiledRegex::Backtracking { .. }
        ));
    }

    /// A budget is only worth having if the patterns it guards fit inside it,
    /// and a `search` that is quadratic in the haystack is what makes them not
    /// fit. This is Linux_SB.har's success assertion — two `(?s)(?=.*…)` over
    /// the whole body, the most expensive lookaround in the corpus — against a
    /// body far larger than the JSON API it is written for.
    ///
    /// The bound is on the clock because the clock is the property: without the
    /// fallback engine's `seek` pre-filter this body takes 66 s to come back
    /// with "no match" (measured: switching the flag off makes this test fail
    /// on the bound after 66.49 s), and on a scheduled run that is
    /// indistinguishable from a hang. The measured time with the pre-filter on
    /// is milliseconds, so the bound is only there to catch the pre-filter
    /// being switched off, not to pin a number.
    #[test]
    fn a_long_body_with_no_payload_answers_instead_of_crawling() {
        let pattern = r#"(?s)(?=.*"ok"\s*:\s*(?:true|1))(?=.*"redirect"\s*:\s*"[^"]*/daily_checkin(?:\?[^"]*)?")"#;
        let pad = "x".repeat(100_000);

        let started = std::time::Instant::now();
        let missing = extract(pattern, &format!(r#"{{"pad": "{pad}"}}"#)).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(missing, None);
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "a body with no payload took {elapsed:?}"
        );

        // The same assertion over the same body, with the payloads present.
        let answered = compile_regex(pattern)
            .unwrap()
            .is_match(&format!(
                r#"{{"ok": true, "redirect": "https://x/daily_checkin", "pad": "{pad}"}}"#
            ))
            .unwrap();
        assert!(answered);
    }

    /// The fallback backtracks, so it can be made to run for a very long time.
    /// It is given a fixed budget and gives up rather than hang — and giving up
    /// is an error, not a `false`, because a match the engine never reached is
    /// not a match it failed to find.
    ///
    /// The tail is `(?!)` rather than a literal, and that is the point. With a
    /// trailing literal the `seek` pre-filter answers first: on the same
    /// alternation ending in `x` over a haystack of `a`s there is no `x`, so
    /// there is nothing to find and the engine never runs — the pre-filter
    /// returns "no match" in a millisecond, which is a *better* answer than the
    /// give-up but leaves the budget untested. `(?!)` is the same alternation
    /// with a tail that can never succeed and no literal to prune on, so the
    /// engine has to try every partition of the run and exhaust the budget to
    /// prove it.
    #[test]
    fn a_runaway_pattern_gives_up_instead_of_running_away() {
        // Two branches that consume different amounts of the same run, and a
        // back-reference to keep them off the linear engine's side: every
        // partition of the haystack has to be tried, and the `(?!)` that would
        // end the search can never succeed.
        let pattern = r"(?:(a)\1|(a)\1\1)+(?!)";
        let message = format!("{:#}", extract(pattern, &"a".repeat(60)).unwrap_err());
        assert!(
            message.contains("gave up"),
            "expected the give-up to be reported, got: {message}"
        );
        assert!(
            message.contains("backtracking"),
            "expected the reason to be named, got: {message}"
        );
        assert!(
            message.contains(&format!("{pattern:?}")),
            "expected the pattern to be named, got: {message}"
        );
        // The same alternation with a tail that *can* succeed answers at once:
        // 60 `a`s are 30 pairs, and then the `x`. The extraction is the first
        // group, one `a`.
        assert_eq!(
            extract(r"(?:(a)\1|(a)\1\1)+x", &format!("{}x", "a".repeat(60))).unwrap(),
            Some(json!("a"))
        );
    }

    /// Python has no POSIX classes: `[[:alpha:]]` is the six characters
    /// `[ : a l p h`, and then a literal `]`. Rust's engine reads the inner
    /// `[` as the start of a POSIX class instead, which compiles and matches
    /// different text — the quietest kind of difference there is.
    #[test]
    fn a_bracket_inside_a_class_is_one_more_member_of_the_set() {
        for name in ["alpha", "digit", "space", "alnum"] {
            let matched = extract(&format!("[[:{name}:]]"), "a").unwrap();
            assert_eq!(matched, None, "`[[:{name}:]]` matched `a`");
        }
        // What it does match: one of those six characters, then a `]`.
        assert_eq!(extract(r"[[:alpha:]]", "a]").unwrap(), Some(json!("a]")));
        assert_eq!(extract(r"[[:alpha:]]", "[]").unwrap(), Some(json!("[]")));
        // A `[` anywhere in a class, and a class that is only a bracket.
        assert_eq!(extract(r"[a[b]", "[").unwrap(), Some(json!("[")));
        assert_eq!(extract(r"[[]", "[").unwrap(), Some(json!("[")));
        // An already-escaped bracket is left as it is.
        assert_eq!(extract(r"[\[a]", "[").unwrap(), Some(json!("[")));
    }

    /// Python lets a backslash escape any character that is not a letter or a
    /// digit, and reads `\<` as a plain `<`. Rust's engine refuses those
    /// escapes, and the fallback engine reads some of them as oniguruma
    /// operators — `\<` and `\>` become word boundaries — so leaving them alone
    /// would match the wrong text on a template that is written correctly.
    #[test]
    fn a_backslash_before_punctuation_is_the_punctuation() {
        // yunyaokz.har verbatim: the look-behind has to be `<b>`, not a word
        // boundary and a `b`.
        assert_eq!(
            extract(r"(?<=累计已签到:  \<b\>)[0-9]+", "累计已签到:  <b>12").unwrap(),
            Some(json!("12"))
        );
        assert_eq!(extract(r"\<b\>", "<b>").unwrap(), Some(json!("<b>")));
        assert_eq!(extract(r"\:", ":").unwrap(), Some(json!(":")));
        assert_eq!(extract(r"a\@b", "a@b").unwrap(), Some(json!("a@b")));
        // Inside a class, and after an escaped backslash, which is a literal
        // backslash and then a literal `<`.
        assert_eq!(extract(r"[\<]", "<").unwrap(), Some(json!("<")));
        assert_eq!(extract(r"\\<", r"\<").unwrap(), Some(json!(r"\<")));
        // The characters Rust's engine does give a meaning to keep their
        // backslash, so `\*` is still a literal star and `\n` is still a
        // newline.
        assert_eq!(extract(r"a\*b", "a*b").unwrap(), Some(json!("a*b")));
        assert!(!compile_regex(r"a\*b").unwrap().is_match("aab").unwrap());
        assert_eq!(extract(r"a\nb", "a\nb").unwrap(), Some(json!("a\nb")));
    }

    /// The oniguruma vocabulary the fallback engine knows and Python does not.
    ///
    /// None of it is a mistake to refuse: `\h` is a horizontal space, `\R` a
    /// line break, `\X` a grapheme and `\p{...}` a Unicode property in that
    /// engine, and Python has no reading of any of them. Left alone, a pattern
    /// QD rejects would start working here.
    ///
    /// The list is measured rather than guessed: over all 6182 regexes in the
    /// 387 qd-today/templates, none of these appears (the single corpus pattern
    /// that is refused — `[^]` in `糖果VR资源网` — is refused for an unclosed
    /// class, which Python refuses as well).
    #[test]
    fn the_oniguruma_vocabulary_python_does_not_have_is_refused() {
        for pattern in [r"\h", r"\R", r"\X", r"\p{L}", r"\K"] {
            assert!(extract(pattern, "a").is_err(), "{pattern}");
        }
        // Named groups are not on that list: both engines take them.
        assert_eq!(
            extract(r#"(?P<num>\d+)"#, "a12").unwrap(),
            Some(json!("12"))
        );
        // Nor is a back-reference by name.
        assert_eq!(
            extract(r"(?P<w>\w)(?P=w)", "aab").unwrap(),
            Some(json!("a"))
        );
    }

    /// `{m,n}` with `m > n` is a syntax error in Python and in Rust's linear
    /// engine — but the fallback engine reads it as a match, so it has to be
    /// refused before either is asked, or such a pattern would quietly start
    /// working here and not in QD.
    #[test]
    fn an_inverted_repetition_range_is_refused() {
        for pattern in [r"a{2,1}", r"a{9,3}", r"\d{10,2}", r"[a-z]{3,2}"] {
            assert!(extract(pattern, "aaa").is_err(), "{pattern}");
        }
        // The neighbouring forms still compile.
        assert_eq!(extract(r"a{1,1}", "aaa").unwrap(), Some(json!("a")));
        assert_eq!(extract(r"a{0,0}", "aaa").unwrap(), Some(json!("")));
        assert_eq!(extract(r"a{10,20}", &"a".repeat(5)).unwrap(), None);
        // A repetition too large for a `u64` is still compared by its digits.
        assert!(extract(r"a{99999999999999999999,1}", "a").is_err());
    }

    /// `\Z` is Python's end of string; Rust's engine calls it `\z` and refuses
    /// `\Z`, so the translation is what keeps both engines reading the same
    /// text.
    #[test]
    fn the_end_of_string_escape_is_read_the_way_python_reads_it() {
        assert_eq!(extract(r#"ab\Z"#, "ab").unwrap(), Some(json!("ab")));
        // Python's `\Z` is the end of the string and not the end of a line.
        assert_eq!(extract(r#"ab\Z"#, "ab\n").unwrap(), None);
        assert_eq!(extract(r#"/a\Z/g"#, "ba").unwrap(), Some(json!(["a"])));
        // Inside a class Python refuses it too, so it is left for both engines
        // to refuse rather than being rewritten into something Python would
        // have read differently.
        assert!(extract(r#"[\Z]"#, "Z").is_err());
    }

    /// `\N{BULLET}` is `•` in Python — inside a class and out — and Python has
    /// no other reading of `\N`.
    ///
    /// Rust's linear engine refuses the escape outright, which was the right
    /// answer, but the fallback engine reads `\N` as "not a newline" and the
    /// name as literal text. That compiles and matches the wrong thing, which
    /// is worse than refusing: reading it properly needs the Unicode name
    /// table, so the translation refuses it instead.
    ///
    /// The refusal is the one gap left in the dialect layer that a library
    /// could close — `unicode_names2::character` reads a name the way
    /// `unicodedata.lookup` does, both of them case-insensitive and neither
    /// accepting the loose `GREEK-SMALL-LETTER-ALPHA` spelling. Two things
    /// stopped it: no template in qd-today writes `\N{...}` (measured: none of
    /// the 6182 patterns, and none of the 387 templates' text), and the crate's
    /// `Unicode-DFS-2016` licence is not on `deny.toml`'s list, so taking the
    /// dependency is a licensing decision as well as 500 KB of tables.
    #[test]
    fn a_named_character_escape_is_refused_rather_than_misread() {
        assert!(extract(r"\N{BULLET}", "•").is_err());
        assert!(extract(r"[\N{BULLET}]", "•").is_err());
        // An escaped backslash is a literal backslash, then a literal `N`.
        assert_eq!(
            extract(r"\\N\{BULLET\}", r"\N{BULLET}").unwrap(),
            Some(json!(r"\N{BULLET}"))
        );
    }

    /// A numeric escape is a character in Python far more often than it is a
    /// back-reference, and both engines only know the back-reference.
    ///
    /// The cases are `sre_parse`'s three, and the values were read off the
    /// reference interpreter rather than inferred: `\012` is the newline,
    /// `\123` is `S`, `\0377` is `\x1f` and a literal `7`, `\128` is a
    /// back-reference and not octal because `8` is not an octal digit, and
    /// `\400` is refused for being above `\377`.
    #[test]
    fn a_numeric_escape_is_read_the_way_python_reads_it() {
        assert_eq!(extract(r"\012", "\n").unwrap(), Some(json!("\n")));
        assert_eq!(extract(r"\123", "S").unwrap(), Some(json!("S")));
        assert_eq!(extract(r"\000", "\0").unwrap(), Some(json!("\0")));
        assert_eq!(extract(r"\0", "\0").unwrap(), Some(json!("\0")));
        // Two octal digits after the zero, and no more: the third is literal.
        assert_eq!(
            extract(r"\0377", "\u{1f}7").unwrap(),
            Some(json!("\u{1f}7"))
        );
        // `\08` is the zero and then a literal `8`.
        assert_eq!(extract(r"\08", "\u{0}8").unwrap(), Some(json!("\u{0}8")));

        // Inside a class every numeric escape is a character, so `[\1]` is the
        // byte and not the group Python has never been told about.
        assert_eq!(extract(r"[\1]", "\u{1}").unwrap(), Some(json!("\u{1}")));
        assert_eq!(extract(r"[\012]", "\n").unwrap(), Some(json!("\n")));
        assert_eq!(extract(r"[\123]", "S").unwrap(), Some(json!("S")));
        assert_eq!(extract(r"[\19]", "\u{1}").unwrap(), Some(json!("\u{1}")));
        assert_eq!(
            extract(r"[\0377]", "\u{1f}").unwrap(),
            Some(json!("\u{1f}"))
        );

        // Above `\377`, and `[\8]`, where there is no octal digit at all:
        // Python refuses both, and so does this.
        assert!(extract(r"\400", "").is_err());
        assert!(extract(r"\777", "").is_err());
        assert!(extract(r"[\400]", "").is_err());
        assert!(extract(r"[\8]", "8").is_err());

        // The back-references the engines already read, left alone.
        assert_eq!(extract(r"(a)\1", "aa").unwrap(), Some(json!("a")));
        assert_eq!(extract(r"(a)(?(1)b)", "ab").unwrap(), Some(json!("a")));
    }

    /// `(?(id)yes|no)` branches on whether another group took part in the
    /// match. The fallback engine implements it, so it runs here; what has to be
    /// refused is the condition Python would not take, or the branch would be
    /// taken on a group QD never had.
    #[test]
    fn a_conditional_group_branches_the_way_python_branches_it() {
        // What QD stores for a pattern with groups is the first group, so the
        // branch is observed through one that wraps the whole thing: `ab` when
        // the group took part, `c` when it did not.
        assert_eq!(
            extract(r"((a)?(?(2)b|c))", "ab").unwrap(),
            Some(json!("ab"))
        );
        assert_eq!(extract(r"((a)?(?(2)b|c))", "bc").unwrap(), Some(json!("c")));
        // A named condition, and the case with no `|else` at all.
        assert_eq!(
            extract(r"((?P<x>a)?(?(x)b|c))", "ab").unwrap(),
            Some(json!("ab"))
        );
        assert_eq!(
            extract(r"((?P<x>a)?(?(x)b|c))", "bc").unwrap(),
            Some(json!("c"))
        );
        assert_eq!(extract(r"(a)(?(1)b)", "ab").unwrap(), Some(json!("a")));
        // A condition that names a group coming later is legal — measured on the
        // reference interpreter, where `(?(2)a|b)(x)(y)` compiles and
        // `(?(n)a|b)(?P<n>x)` does not — but the branch it takes is always the
        // `|no` one, because the group has not taken part yet when the condition
        // is read. Python matches `bxy` here and not `axy`, and so does this.
        assert_eq!(
            extract(r"(?(2)a|b)(x)(y)", "bxy").unwrap(),
            Some(json!("x"))
        );
        assert_eq!(extract(r"(?(2)a|b)(x)(y)", "axy").unwrap(), None);

        // A group number the pattern does not have, group 0, a name that was
        // never defined, and a name defined only after the branch.
        for pattern in [
            r"(a)(?(2)b|c)",
            r"(?(1)a|b)",
            r"(a)(?(0)b|c)",
            r"(a)(?(a)b|c)",
            r"(?(n)a|b)(?P<n>x)",
        ] {
            let error = extract(pattern, "ab").unwrap_err().to_string();
            assert!(error.contains("conditional group"), "{pattern}: {error}");
        }
        // An assertion and a quoted name are refused by the fallback engine as
        // well, so those patterns stay refused without a condition to check.
        assert!(extract(r"(a)(?(?=a)b|c)", "ab").is_err());
        assert!(extract(r"(a)(?('a')b|c)", "ab").is_err());

        // A `(` inside a `(?x)` comment is not a group, so the number this layer
        // counts for a name comes out one too high here — Python reads this
        // pattern and calls `x` group 1. A wrong number is a wrong branch, which
        // is worse than an error, so every condition is judged against the
        // engine's own group count and capture names, and this one is refused
        // rather than branched on group 2.
        let error = extract("(?x)# (\n(?P<x>a)?(?(x)b|c)", "ab")
            .unwrap_err()
            .to_string();
        assert!(error.contains("conditional group"), "{error}");
        // The number a condition is written as is passed through untouched, so
        // the same pattern with a number is judged the same way — and Python
        // refuses it too (`invalid group reference 3`, because the `(` in the
        // comment is not a group there either).
        let error = extract("(?x)# (\n((a)?(?(3)b|c))", "ab")
            .unwrap_err()
            .to_string();
        assert!(error.contains("group 3"), "{error}");
        // The name check is what catches a miscount that still lands on a group
        // the engine has: here the comment's `(` pushes `x` to number 2, and
        // group 2 is `y` — the branch would be taken on the wrong group. Python
        // reads this pattern and matches `ac`; refusing is the loud side of that.
        let error = extract("(?x)# (\n(?P<x>a)(?P<y>b)?(?(x)c|d)", "ac")
            .unwrap_err()
            .to_string();
        assert!(error.contains("naming `x`"), "{error}");
    }

    /// Three things Python refuses and the engines accept, which is the
    /// direction that matters: a template written here would work and then fail
    /// in QD.
    #[test]
    fn what_python_refuses_the_engines_will_not_take() {
        // `(?<name>...)` is PCRE's spelling; Python spells it `(?P<name>...)`.
        assert!(extract(r"(?<n>a)", "a").is_err());
        assert!(extract(r"(?<>)", "").is_err());
        // The backtracking verbs.
        assert!(extract(r"(*FAIL)a", "a").is_err());
        assert!(extract(r"a(*SKIP)b", "ab").is_err());
        // A flag turned off outside a group, where Python wants `(?-i:...)`.
        assert!(extract(r"(?-i)a", "a").is_err());
        // The ASCII flag, which neither engine can express.
        let error = extract(r"(?a)\w", "a").unwrap_err().to_string();
        assert!(error.contains("ASCII flag"), "{error}");
        assert!(extract(r"(?a:\w)", "a").is_err());

        // What Python and the engines do agree on is untouched.
        assert_eq!(extract(r"(?<=a)b", "ab").unwrap(), Some(json!("b")));
        assert_eq!(extract(r"(?<!x)b", "ab").unwrap(), Some(json!("b")));
        assert_eq!(extract(r"(?i)A", "a").unwrap(), Some(json!("a")));
        assert_eq!(extract(r"(?s).", "\n").unwrap(), Some(json!("\n")));
        assert_eq!(extract(r"(?u)a", "a").unwrap(), Some(json!("a")));
        assert_eq!(extract(r"(?x) a b", "ab").unwrap(), Some(json!("ab")));
        assert_eq!(extract(r"(?:a)b", "ab").unwrap(), Some(json!("ab")));
        assert_eq!(extract(r"(?P<n>a)", "a").unwrap(), Some(json!("a")));
    }

    /// Python reads `{` as a repetition only when a well-formed one follows,
    /// so a JSON snippet is literal text; Rust's engine refuses those outright
    /// unless the brace is escaped.
    #[test]
    fn literal_braces_are_read_the_way_python_reads_them() {
        assert_eq!(
            extract(r#"{"total":(\d+)"#, r#"{"total":12}"#).unwrap(),
            Some(json!("12"))
        );
        assert_eq!(
            extract(r#"{"a":1}"#, r#"{"a":1}"#).unwrap(),
            Some(json!("{\"a\":1}"))
        );
        // A lone brace, and an escape that owns its brace.
        assert_eq!(extract(r#"a{"#, "a{").unwrap(), Some(json!("a{")));
        assert_eq!(extract(r#"\{\}"#, "{}").unwrap(), Some(json!("{}")));
        // A class is a set of characters in both engines.
        assert_eq!(extract(r#"[{}]"#, "{").unwrap(), Some(json!("{")));
        // Escaping the literal braces must not disturb the real repetitions.
        assert_eq!(extract(r#"a{2}"#, "aaa").unwrap(), Some(json!("aa")));
        assert_eq!(extract(r#"a{2,}"#, "aaa").unwrap(), Some(json!("aaa")));
        assert_eq!(extract(r#"a{2,3}"#, "aaaa").unwrap(), Some(json!("aaa")));
        assert_eq!(extract(r#"\d{1,3}"#, "1234").unwrap(), Some(json!("123")));
    }

    /// `{,n}` and `{,}` omit their minimum in Python; Rust has no shorthand for
    /// that, so it has to be written out.
    #[test]
    fn an_omitted_repetition_minimum_is_written_out() {
        assert_eq!(extract(r#"a{,2}"#, "aaa").unwrap(), Some(json!("aa")));
        assert_eq!(extract(r#"a{,}"#, "aaa").unwrap(), Some(json!("aaa")));
        // Still a repetition, so still refused by both engines.
        assert!(extract(r#"a{2,1}"#, "aaa").is_err());
    }

    /// The encoding QD's `utils.decode` picks, source by source. Each case
    /// isolates one: a wrong order would show up as mojibake rather than as a
    /// missing feature.
    #[test]
    fn a_response_is_read_in_the_encoding_it_declares() {
        let chinese = encoding_rs::GBK
            .encode("中文签到成功获得积分")
            .0
            .into_owned();
        let content_type = |value: &str| vec![("content-type".to_string(), value.to_string())];

        assert_eq!(
            decode_body(&chinese, &content_type("text/html; charset=gb2312")),
            "中文签到成功获得积分"
        );
        // The same bytes when the header says nothing and the body declares the
        // character set itself.
        let mut page = br#"<html><head><meta charset="gb2312"></head><body>"#.to_vec();
        page.extend_from_slice(&chinese);
        assert!(decode_body(&page, &[]).ends_with("中文签到成功获得积分"));

        // `ISO-8859-1` is what an HTTP stack writes when nobody decided, so QD
        // discards it and reads the declaration behind it.
        assert!(
            decode_body(&page, &content_type("text/html; charset=ISO-8859-1"))
                .ends_with("中文签到成功获得积分")
        );

        // With nothing declared, the detector reads what is there.
        assert_eq!(decode_body(&chinese, &[]), "中文签到成功获得积分");
        assert_eq!(decode_body("中文".as_bytes(), &[]), "中文");
        // A byte the encoding cannot carry becomes U+FFFD rather than an error,
        // which is Python's `errors="replace"`.
        assert!(
            decode_body(&[0xff], &content_type("text/html; charset=utf-8")).contains('\u{fffd}')
        );
    }

    /// XML's declaration is the third source, and QD reads it only at the start
    /// of the document.
    #[test]
    fn an_xml_declaration_is_read_as_an_encoding_source() {
        let chinese = encoding_rs::GBK.encode("中文").0.into_owned();
        let mut document = br#"<?xml version="1.0" encoding="gb2312"?><a>"#.to_vec();
        document.extend_from_slice(&chinese);
        assert!(decode_body(&document, &[]).ends_with("中文"));
    }

    #[test]
    fn only_a_response_that_says_it_is_an_image_is_one() {
        let header = |value: &str| vec![("Content-Type".to_string(), value.to_string())];
        assert!(is_image_response(&header("image/png")));
        assert!(is_image_response(&header("IMAGE/JPEG; charset=binary")));
        assert!(!is_image_response(&header("text/html; charset=utf-8")));
        assert!(!is_image_response(&[]));
    }

    /// QD hands a rule the base64 of an image response, which is what keeps a
    /// captcha readable to the extraction that follows it.
    #[tokio::test]
    async fn an_image_response_reaches_a_rule_as_base64() {
        let bytes: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let published = bytes.clone();
        let app = Router::new().route(
            "/captcha.png",
            get(move || {
                let bytes = published.clone();
                async move { ([("content-type", "image/png")], bytes) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let har = QdHar::parse(json!({"log": {"version": "1.2", "entries": [{
            "checked": true,
            "request": {"method": "GET", "url": format!("http://{address}/captcha.png")},
            "extract_variables": [{"name": "captcha", "re": "(.+)", "from": "content"}]
        }]}}))
        .unwrap();
        let program = QdProgram::compile(&har).unwrap();
        let executor = local_executor();
        let mut context = ExecutionContext::new(BTreeMap::new());

        executor.execute(&program, &mut context).await.unwrap();

        assert_eq!(
            context.variables.get("captcha"),
            Some(&json!(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &bytes
            )))
        );
    }

    /// QD stores the exception's text as the variable when a pattern will not
    /// compile, and the run keeps going; the same shape here, with this
    /// engine's message.
    #[tokio::test]
    async fn a_broken_extraction_pattern_becomes_a_variable_not_a_failed_run() {
        let app = Router::new().route("/page", get(|| async { "hello" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let har = QdHar::parse(json!({"log": {"version": "1.2", "entries": [{
            "checked": true,
            "request": {"method": "GET", "url": format!("http://{address}/page")},
            "extract_variables": [
                {"name": "broken", "re": "(unclosed", "from": "content"},
                {"name": "fine", "re": "(hel+)", "from": "content"}
            ]
        }]}}))
        .unwrap();
        let program = QdProgram::compile(&har).unwrap();
        let executor = local_executor();
        let mut context = ExecutionContext::new(BTreeMap::new());

        let results = executor.execute(&program, &mut context).await.unwrap();

        assert_eq!(results.len(), 1);
        let stored = context
            .variables
            .get("broken")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        assert!(stored.contains("invalid QD regular expression"), "{stored}");
        // The rule after the broken one still ran.
        assert_eq!(context.variables.get("fine"), Some(&json!("hell")));
    }
}
