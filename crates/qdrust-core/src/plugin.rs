use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize, Serializer};
use tokio::io::AsyncWriteExt;
use tracing::info;

use crate::executor::{OutboundPolicy, guarded_client_for_url};

pub const PLUGIN_API_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginManifest {
    pub api_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub capabilities: Vec<PluginCapability>,
}

impl PluginManifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.api_version == PLUGIN_API_VERSION,
            "unsupported plugin API version"
        );
        ensure!(valid_plugin_id(&self.id), "invalid plugin id");
        ensure!(!self.name.trim().is_empty(), "plugin name is empty");
        ensure!(!self.version.trim().is_empty(), "plugin version is empty");
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginCapability {
    Network,
    ReadFile,
    WriteFile,
    Environment,
}

impl PluginCapability {
    /// Wire name of the capability, matching the serde `snake_case` encoding
    /// used in manifests, configs and responses.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::ReadFile => "read_file",
            Self::WriteFile => "write_file",
            Self::Environment => "environment",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginRequest {
    pub plugin_id: String,
    pub action: String,
    pub query: BTreeMap<String, String>,
}

impl PluginRequest {
    pub fn from_api_url(value: &str) -> Result<Self> {
        let url = reqwest::Url::parse(value).context("invalid api plugin URL")?;
        ensure!(url.scheme() == "api", "plugin URL must use api scheme");
        let plugin_id = url.host_str().context("plugin id is missing")?.to_string();
        ensure!(valid_plugin_id(&plugin_id), "invalid plugin id");
        let action = url.path().trim_matches('/').to_string();
        ensure!(!action.is_empty(), "plugin action is missing");
        Ok(Self {
            plugin_id,
            action,
            query: url.query_pairs().into_owned().collect(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

/// Wire envelope a subprocess plugin writes to stdout. `capabilities_used`
/// reports which capabilities the call exercised so the host can enforce
/// ADR-0006 ("未声明 capability 的调用在宿主层拒绝"): anything reported but
/// not declared in the manifest rejects the whole call. The field is optional
/// on the wire - plugins written before capability reporting keep working and
/// simply report nothing.
///
/// The report is cooperative: a hostile plugin can lie about what it did, so
/// OS-level sandboxing remains the real boundary. This envelope keeps honest
/// plugins inside their declaration and makes violations fail loudly.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginCallResponse {
    #[serde(flatten)]
    pub response: PluginResponse,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities_used: Vec<PluginCapability>,
}

#[derive(Debug)]
pub struct SubprocessPlugin {
    manifest: PluginManifest,
    executable: PathBuf,
    arguments: Vec<String>,
    output_limit: usize,
}

impl SubprocessPlugin {
    pub fn new(
        manifest: PluginManifest,
        executable: impl AsRef<Path>,
        arguments: Vec<String>,
    ) -> Result<Self> {
        manifest.validate()?;
        Ok(Self {
            manifest,
            executable: executable.as_ref().to_path_buf(),
            arguments,
            output_limit: 1024 * 1024,
        })
    }

    /// Build a plugin from a single stored command string, split on whitespace
    /// into a program plus its arguments. No shell is involved, so a program
    /// path containing spaces cannot be quoted - the stored command is
    /// validated to reject shell operators before it reaches the database.
    /// A command without whitespace keeps today's "command is the executable"
    /// behaviour.
    pub fn from_command(manifest: PluginManifest, command: &str) -> Result<Self> {
        let mut parts = command.split_whitespace();
        let program = parts.next().context("plugin command is empty")?;
        Self::new(manifest, program, parts.map(str::to_string).collect())
    }

    #[cfg(test)]
    fn command_parts(&self) -> (&Path, &[String]) {
        (&self.executable, &self.arguments)
    }

    /// Host-side enforcement of ADR-0006: a subprocess plugin reports the
    /// capabilities each call exercised via `capabilities_used`, and anything
    /// not declared in the manifest rejects the whole call. Both call paths -
    /// template execution through the registry and the ad-hoc
    /// `/api/v1/plugins/{id}/invoke` route - go through this check.
    fn ensure_declared_capabilities(
        &self,
        request: &PluginRequest,
        used: &[PluginCapability],
    ) -> Result<()> {
        for capability in used {
            ensure!(
                self.manifest.capabilities.contains(capability),
                "plugin {}/{} used undeclared capability: {} (declared: {})",
                request.plugin_id,
                request.action,
                capability.as_str(),
                capability_list(&self.manifest.capabilities),
            );
        }
        Ok(())
    }
}

impl Plugin for SubprocessPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn call<'a>(
        &'a self,
        request: &'a PluginRequest,
    ) -> Pin<Box<dyn Future<Output = Result<PluginResponse>> + Send + 'a>> {
        Box::pin(async move {
            let mut child = tokio::process::Command::new(&self.executable)
                .args(&self.arguments)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .with_context(|| {
                    format!("cannot start plugin process: {}", self.executable.display())
                })?;
            let mut payload = serde_json::to_vec(request)?;
            payload.push(b'\n');
            child
                .stdin
                .take()
                .context("plugin stdin is unavailable")?
                .write_all(&payload)
                .await?;
            let output = child.wait_with_output().await?;
            ensure!(output.status.success(), "plugin process failed");
            ensure!(
                output.stdout.len() <= self.output_limit,
                "plugin output limit exceeded"
            );
            let envelope: PluginCallResponse =
                serde_json::from_slice(&output.stdout).context("invalid plugin response")?;
            self.ensure_declared_capabilities(request, &envelope.capabilities_used)?;
            Ok(envelope.response)
        })
    }
}

pub trait Plugin: Send + Sync {
    fn manifest(&self) -> &PluginManifest;

    fn call<'a>(
        &'a self,
        request: &'a PluginRequest,
    ) -> Pin<Box<dyn Future<Output = Result<PluginResponse>> + Send + 'a>>;
}

#[derive(Default)]
pub struct PluginRegistry {
    plugins: BTreeMap<String, Arc<dyn Plugin>>,
}

impl PluginRegistry {
    pub fn register(&mut self, plugin: Arc<dyn Plugin>) -> Result<()> {
        plugin.manifest().validate()?;
        let id = plugin.manifest().id.clone();
        ensure!(
            !self.plugins.contains_key(&id),
            "plugin is already registered"
        );
        self.plugins.insert(id, plugin);
        Ok(())
    }

    /// Ids of every registered plugin, in a stable (sorted) order. Used to make
    /// "plugin unavailable" failures self-explanatory: the message lists what
    /// *was* wired into the run.
    pub fn ids(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }

    pub async fn call(&self, value: &str, timeout: Duration) -> Result<PluginResponse> {
        let request = PluginRequest::from_api_url(value)?;
        let plugin = self.plugins.get(&request.plugin_id).with_context(|| {
            format!(
                "plugin unavailable: {}/{} (registered: {})",
                request.plugin_id,
                request.action,
                join_ids(&self.ids())
            )
        })?;
        let started = Instant::now();
        let outcome = tokio::time::timeout(timeout, plugin.call(&request)).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let (plugin_id, action) = (request.plugin_id.as_str(), request.action.as_str());
        // Name the plugin and action in every failure: "plugin unavailable" or
        // "plugin call timed out" alone does not say which step broke.
        let result = match outcome {
            Err(_) => bail!("plugin call timed out: {plugin_id}/{action} after {elapsed_ms} ms"),
            Ok(outcome) => {
                outcome.with_context(|| format!("plugin call failed: {plugin_id}/{action}"))
            }
        };
        match &result {
            Ok(response) => info!(
                plugin_id,
                action,
                status = response.status,
                elapsed_ms,
                "plugin call finished"
            ),
            Err(err) => info!(
                plugin_id,
                action,
                error = %err,
                elapsed_ms,
                "plugin call failed"
            ),
        }
        result
    }
}

/// Render the registered plugin ids for diagnostics; `<none>` keeps the empty
/// registry case readable instead of showing a bare comma list.
fn join_ids(ids: &[String]) -> String {
    if ids.is_empty() {
        return "<none>".to_string();
    }
    ids.join(",")
}

/// Render declared capabilities for diagnostics; `<none>` keeps the
/// nothing-declared case readable instead of showing a bare comma list.
fn capability_list(capabilities: &[PluginCapability]) -> String {
    if capabilities.is_empty() {
        return "<none>".to_string();
    }
    capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// QD's `NOTEPAD_LIMIT`, default 20: the most slots one account may hold.
///
/// A constant rather than a setting, like the content limit below: 20 slots is
/// already far past what a template needs, and the number is not something a
/// deployment has ever had to change. Every path a slot can be born on — a
/// template's `f=add` and the notepad page's "new slot" — enforces it from this
/// one value.
pub const NOTEPAD_LIMIT: usize = 20;

/// The most text one slot may hold, as written by a template.
///
/// QD puts no bound here at all, so this is a deliberate difference: a template
/// writes without one, and every other limit in this repo (`response_limit`,
/// `request_limit`, the 300s run deadline) exists for the same reason. The value
/// matches the default `response_limit`, so a value that can be stored can also
/// be read back through a normal step — a larger cap would let a template write
/// something it can never read.
///
/// The notepad page reads such a value back whatever its size, but it writes
/// through a smaller bound of its own — see `NOTEPAD_EDIT_LIMIT` in
/// `qdrust-server`, and the note there on why the two cannot be one number.
const NOTEPAD_CONTENT_LIMIT: usize = 5 * 1024 * 1024;

/// QD writes both of these as the response body of a failed notepad call, so a
/// template author sees one or the other depending on which route they hit. The
/// comma differs between the two in QD itself (`util.py`), which is why the two
/// spellings are kept apart here rather than normalised to one.
const NOTEPAD_INCOMPLETE: &str = "参数不完整，请确认";
const NOTEPAD_INCOMPLETE_LIST: &str = "参数不完整, 请确认";

/// What one notepad slot holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotepadSlot {
    /// No row for this slot. QD auto-creates slot 1 on first use and refuses
    /// any other missing slot with 「记事本不存在」, so this is a branch the
    /// caller has to take rather than an error.
    Missing,
    /// A row exists. `None` is QD's NULL content — the state a slot it created
    /// itself is in, and the state `append` tells apart from `""` (it joins with
    /// a CRLF only when something is already stored).
    Stored(Option<String>),
}

/// Cross-run storage behind `api://util/toolbox/notepad`.
///
/// QD keeps a notepad as a `(userid, notepadid, content)` row and lets a
/// template read one, overwrite it, or append to it. Only the caller knows whose
/// notepad that is and where it lives: `qdrust-core` cannot reach the server's
/// database, and the CLI has no database at all. So the plugin is handed this
/// instead, and **whose** notepad it is belongs to the implementation, not to a
/// parameter here — which is also what keeps the account out of the URL.
pub trait NotepadStore: Send + Sync + std::fmt::Debug {
    fn read<'a>(
        &'a self,
        slot: i64,
    ) -> Pin<Box<dyn Future<Output = Result<NotepadSlot>> + Send + 'a>>;

    /// Create or overwrite one slot. `None` is QD's NULL content, which is what
    /// a freshly created slot holds until something writes to it.
    fn write<'a>(
        &'a self,
        slot: i64,
        content: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

    /// Every slot this account holds, in any order.
    fn slots<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Vec<i64>>> + Send + 'a>>;

    fn remove<'a>(&'a self, slot: i64) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

/// The built-in `util` plugin: every QD `api://util/...` route that is not
/// handled by a registered plugin.
///
/// All but two actions are pure computation. `dddd/*` forwards to a DdddOCR
/// server the template names through `_server` — that forward leaves this
/// process, so it carries the shared guard rather than a client of this
/// module's own. `toolbox/notepad` is the other: it keeps state across runs, so
/// it needs a [`NotepadStore`] handed in from outside. Everything else stays
/// pure, which is why the notepad arrives as an injected dependency rather than
/// this plugin growing a database of its own.
pub struct UtilityPlugin {
    manifest: PluginManifest,
    /// Snapshotted at construction, because the executor is built per run from
    /// the live settings: the snapshot *is* that run's posture. Read per run
    /// for the same reason the run policy is — a switch that needs a restart
    /// reads as a switch that does not work.
    policy: OutboundPolicy,
    /// `None` in the CLI and in most tests, where there is no database to keep
    /// a notepad in. The action reports that rather than pretending to store
    /// anything.
    notepad: Option<Arc<dyn NotepadStore>>,
}

impl UtilityPlugin {
    pub fn with_policy(policy: OutboundPolicy) -> Self {
        Self {
            manifest: PluginManifest {
                api_version: PLUGIN_API_VERSION,
                id: "util".into(),
                name: "Built-in utilities".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                capabilities: Vec::new(),
            },
            policy,
            notepad: None,
        }
    }

    /// Attach the cross-run storage `api://util/toolbox/notepad` writes to. Kept
    /// separate from [`Self::with_policy`] so the wiring that hands the plugin
    /// its outbound policy stays one call site the SSRF guard test can pin.
    pub fn with_notepad(mut self, notepad: Option<Arc<dyn NotepadStore>>) -> Self {
        self.notepad = notepad;
        self
    }
}

impl Default for UtilityPlugin {
    /// Closed posture — every relaxation off, which is also
    /// [`OutboundPolicy::default`]. The executor builds this plugin from its
    /// own options every time, so reaching this implementation means a caller
    /// with no policy to offer: tests, and the CLI's `QdExecutor::new`.
    fn default() -> Self {
        Self::with_policy(OutboundPolicy::default())
    }
}

impl Plugin for UtilityPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn call<'a>(
        &'a self,
        request: &'a PluginRequest,
    ) -> Pin<Box<dyn Future<Output = Result<PluginResponse>> + Send + 'a>> {
        Box::pin(async move {
            match request.action.as_str() {
                // QD registers three delay routes: `/util/delay` (seconds via the
                // `seconds` query) plus `/util/delay/(\d+)` and
                // `/util/delay/(\d+\.\d+)` (seconds in the path). Its HAR editor's
                // "insert delay" button emits the path form `api://util/delay/3`,
                // so both spellings must work. The action is the whole path here,
                // so a path-form call arrives as `delay/<n>`.
                action if action == "delay" || action.starts_with("delay/") => {
                    let raw = match action.strip_prefix("delay/") {
                        Some(raw) => raw,
                        None => request
                            .query
                            .get("seconds")
                            .map(String::as_str)
                            .unwrap_or("0"),
                    };
                    // QD wraps `float(...)` in try/except and answers
                    // "Error, delay 0.0 second.": an unreadable value is a
                    // reply, not a failed step, so the run continues.
                    let Ok(parsed) = raw.parse::<f64>() else {
                        return Ok(PluginResponse {
                            status: 200,
                            headers: BTreeMap::new(),
                            body: b"Error, delay 0.0 second.".to_vec(),
                        });
                    };
                    if !parsed.is_finite() {
                        return Ok(PluginResponse {
                            status: 200,
                            headers: BTreeMap::new(),
                            body: b"Error, delay 0.0 second.".to_vec(),
                        });
                    }
                    // QD clamps a negative delay to zero and then sleeps.
                    let seconds = parsed.max(0.0);
                    // QD clamps at `delay_max_timeout` and sleeps the boundary
                    // (300 s), answering with its "Error, limited by ..." text.
                    // Sleeping the boundary here would only ever end as the
                    // caller's own plugin timeout, which reads as a network
                    // fault rather than an over-long delay, so the refusal is
                    // kept and named with the same boundary.
                    ensure!(
                        seconds <= DELAY_MAX_SECONDS,
                        "delay must be at most {DELAY_MAX_SECONDS} seconds"
                    );
                    tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        // QD writes `f"delay {seconds} second."` here, and that
                        // line is the `content` a template extracts.
                        body: format!("delay {} second.", qd_float(seconds)).into_bytes(),
                    })
                }
                "timestamp" => {
                    // QD 兼容（qd web/handlers/util.py TimeStampHandler）：`ts`（秒，
                    // 可带小数）或 `dt`（配 `form`，Python strptime 语法）给出时间，
                    // 两者都缺省时用当前时间；返回 QD 的中文键 JSON，供
                    // success_asserts 的 "\"状态\": \"200\"" 与 extract_variables 的
                    // "\"时间戳\": \"(.*)\"" 之类规则按原样匹配。
                    let ts_arg = request.query.get("ts").map(String::as_str).unwrap_or("");
                    let dt_arg = request.query.get("dt").map(String::as_str).unwrap_or("");
                    let format = request
                        .query
                        .get("form")
                        .map(String::as_str)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("%Y-%m-%d %H:%M:%S");
                    let body = timestamp_body(ts_arg, dt_arg, format)?;
                    let mut headers = BTreeMap::new();
                    headers.insert(
                        "content-type".to_string(),
                        "application/json; charset=UTF-8".to_string(),
                    );
                    Ok(PluginResponse {
                        status: 200,
                        headers,
                        body: body.into_bytes(),
                    })
                }
                "unicode" => {
                    // QD 兼容（qd web/handlers/util.py UniCodeHandler）：content
                    // 参数做 unicode_escape 解码（\uXXXX/\xNN → 字符，普通文本
                    // 原样），返回与 QD 相同的缩进 JSON，供 success_asserts 的
                    // "\"状态\": \"200\"" 与 extract_variables 的
                    // "\"转换后\": \"(.*)\"" 规则按原样匹配。
                    let content = request
                        .query
                        .get("content")
                        .or_else(|| request.query.get("text"))
                        .map(String::as_str)
                        .unwrap_or("");
                    let html_unescape = request
                        .query
                        .get("html_unescape")
                        .map(String::as_str)
                        .map(strtobool)
                        .unwrap_or(false);
                    let mut converted = crate::expression::conver2unicode(content);
                    if html_unescape {
                        converted = html_escape::decode_html_entities(&converted).to_string();
                    }
                    let value = serde_json::to_string(&converted)?;
                    let mut headers = BTreeMap::new();
                    headers.insert(
                        "content-type".to_string(),
                        "application/json; charset=UTF-8".to_string(),
                    );
                    Ok(PluginResponse {
                        status: 200,
                        headers,
                        body: format!("{{\n    \"转换后\": {value},\n    \"状态\": \"200\"\n}}")
                            .into_bytes(),
                    })
                }
                "regex" => {
                    // QD 兼容（qd web/handlers/util.py UtilRegexHandler）：`data`
                    // 原文、`p` 正则，等价于 `re.findall(p, data, re.IGNORECASE)`，
                    // 按 findall 的组语义（无组取整个匹配、一组取该组、多组取元组）
                    // 编号成表，返回 QD 同款缩进 JSON；正则非法时只有「状态」键，
                    // 与 QD 的 except 分支一致。
                    let data = request.query.get("data").map(String::as_str).unwrap_or("");
                    let pattern = request.query.get("p").map(String::as_str).unwrap_or("");
                    let body = match crate::executor::python_findall(pattern, data) {
                        Ok(matches) => qd_json(&QdJson::object(vec![
                            (
                                "数据".to_string(),
                                QdJson::object(
                                    matches
                                        .iter()
                                        .enumerate()
                                        .map(|(index, value)| {
                                            ((index + 1).to_string(), QdJson::value(value.clone()))
                                        })
                                        .collect(),
                                ),
                            ),
                            ("状态".to_string(), QdJson::value(serde_json::json!("OK"))),
                        ])),
                        Err(err) => qd_json(&qd_status(&err.to_string())),
                    }?;
                    let mut headers = BTreeMap::new();
                    headers.insert(
                        "content-type".to_string(),
                        "application/json; charset=UTF-8".to_string(),
                    );
                    Ok(PluginResponse {
                        status: 200,
                        headers,
                        body: body.into_bytes(),
                    })
                }
                "base64" => {
                    let text = request.query.get("text").map(String::as_str).unwrap_or("");
                    let operation = request
                        .query
                        .get("op")
                        .map(String::as_str)
                        .unwrap_or("encode");

                    let result = match operation {
                        "encode" => {
                            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, text)
                        }
                        "decode" => {
                            let decoded = base64::Engine::decode(
                                &base64::engine::general_purpose::STANDARD,
                                text,
                            )
                            .context("invalid base64")?;
                            String::from_utf8(decoded)
                                .context("decoded base64 is not valid UTF-8")?
                        }
                        _ => bail!("unsupported base64 operation: {operation}"),
                    };

                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: result.into_bytes(),
                    })
                }
                "hash" => {
                    use sha1::Digest as Sha1Digest;

                    let text = request.query.get("text").map(String::as_str).unwrap_or("");
                    let algorithm = request
                        .query
                        .get("algo")
                        .map(String::as_str)
                        .unwrap_or("md5");

                    let result = match algorithm {
                        "md5" => {
                            let digest = md5::compute(text.as_bytes());
                            format!("{:x}", digest)
                        }
                        "sha1" => {
                            let digest = sha1::Sha1::digest(text.as_bytes());
                            hex::encode(digest)
                        }
                        "sha256" => {
                            let digest = sha2::Sha256::digest(text.as_bytes());
                            hex::encode(digest)
                        }
                        "sha512" => {
                            let digest = sha2::Sha512::digest(text.as_bytes());
                            hex::encode(digest)
                        }
                        _ => bail!("unsupported hash algorithm: {algorithm}"),
                    };

                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: result.into_bytes(),
                    })
                }
                "totp" => {
                    // Computed locally so a 2FA secret never has to be sent to an
                    // external API. `secret` is the base32 value from the setup
                    // page; `t` exists for replay and tests.
                    let secret = request
                        .query
                        .get("secret")
                        .map(String::as_str)
                        .unwrap_or("");
                    let digits = request
                        .query
                        .get("digits")
                        .and_then(|value| value.parse::<u32>().ok())
                        .unwrap_or(6);
                    let period = request
                        .query
                        .get("period")
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(30);
                    let algorithm = request
                        .query
                        .get("algo")
                        .map(String::as_str)
                        .unwrap_or("sha1");
                    let at = request
                        .query
                        .get("t")
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or_else(|| chrono::Utc::now().timestamp().max(0) as u64);
                    let result = crate::totp::code(secret, digits, period, algorithm, at)?;

                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: result.into_bytes(),
                    })
                }
                "uuid" => {
                    let namespace = request
                        .query
                        .get("namespace")
                        .map(String::as_str)
                        .unwrap_or("");
                    let name = request.query.get("name").map(String::as_str).unwrap_or("");

                    let result = if !namespace.is_empty() && !name.is_empty() {
                        let ns_uuid =
                            uuid::Uuid::parse_str(namespace).unwrap_or(uuid::Uuid::NAMESPACE_URL);
                        uuid::Uuid::new_v5(&ns_uuid, name.as_bytes()).to_string()
                    } else {
                        uuid::Uuid::new_v4().to_string()
                    };

                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: result.into_bytes(),
                    })
                }
                "random" => {
                    use rand::Rng;

                    let kind = request
                        .query
                        .get("type")
                        .map(String::as_str)
                        .unwrap_or("int");
                    let min = request
                        .query
                        .get("min")
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(0);
                    let max = request
                        .query
                        .get("max")
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(100);

                    let mut rng = rand::thread_rng();
                    let result = match kind {
                        "int" => rng.gen_range(min..=max).to_string(),
                        "float" => {
                            let value: f64 = rng.gen_range(min as f64..=max as f64);
                            value.to_string()
                        }
                        _ => bail!("unsupported random type: {kind}"),
                    };

                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: result.into_bytes(),
                    })
                }
                "urlencode" => {
                    let text = request.query.get("text").map(String::as_str).unwrap_or("");
                    let operation = request
                        .query
                        .get("op")
                        .map(String::as_str)
                        .unwrap_or("encode");

                    let result = match operation {
                        "encode" => percent_encoding::utf8_percent_encode(
                            text,
                            percent_encoding::NON_ALPHANUMERIC,
                        )
                        .to_string(),
                        "decode" => percent_encoding::percent_decode_str(text)
                            .decode_utf8()
                            .context("invalid URL encoding")?
                            .to_string(),
                        _ => bail!("unsupported urlencode operation: {operation}"),
                    };

                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: result.into_bytes(),
                    })
                }
                "json" => {
                    let text = request.query.get("text").map(String::as_str).unwrap_or("");
                    let operation = request
                        .query
                        .get("op")
                        .map(String::as_str)
                        .unwrap_or("parse");

                    let result = match operation {
                        "parse" => {
                            let _: serde_json::Value =
                                serde_json::from_str(text).context("invalid JSON")?;
                            text.to_string()
                        }
                        "stringify" => {
                            let value: serde_json::Value = serde_json::from_str(text)?;
                            serde_json::to_string(&value)?
                        }
                        "pretty" => {
                            let value: serde_json::Value = serde_json::from_str(text)?;
                            serde_json::to_string_pretty(&value)?
                        }
                        _ => bail!("unsupported json operation: {operation}"),
                    };

                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: result.into_bytes(),
                    })
                }
                action if action.starts_with("dddd/") => {
                    // DdddOCR verification code recognition. QD proxies this to an
                    // external DdddOCR HTTP server; here we forward to a configured
                    // base URL (see api://util/dddd/ocr/... ?_server=... or env).
                    let base = request
                        .query
                        .get("_server")
                        .cloned()
                        .or_else(|| std::env::var("QDRUST_DDDDOCR_SERVER").ok())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "DdddOCR is not configured: set _server query param or QDRUST_DDDDOCR_SERVER"
                            )
                        })?;
                    let server = reqwest::Url::parse(&base)
                        .context("invalid DdddOCR server URL")?
                        .join(action.trim_start_matches("dddd/"))
                        .context("invalid DdddOCR route")?;
                    // The target arrives from the template (`_server`), so it
                    // goes through the same gate as every other outbound
                    // request: address classification, the admin switches, and
                    // a pinned address. Three of those were absent while this
                    // built its own client — including the redirect policy, so
                    // this path followed up to ten hops where the rest of the
                    // process followed none.
                    //
                    // No context wrapper on the failure: the guard's own
                    // sentence is the one that tells an operator what happened,
                    // and burying it under "server is not reachable" would read
                    // as a network problem instead of a refusal.
                    let client =
                        guarded_client_for_url(server.as_str(), &self.policy, None).await?;
                    let mut request_builder = client.request(
                        reqwest::Method::from_bytes(
                            request
                                .query
                                .get("_method")
                                .map(String::as_str)
                                .unwrap_or("POST")
                                .as_bytes(),
                        )
                        .unwrap_or(reqwest::Method::POST),
                        server,
                    );
                    if let Some(body) = request.query.get("body") {
                        request_builder = request_builder.body(body.clone());
                        request_builder = request_builder.header(
                            reqwest::header::CONTENT_TYPE,
                            reqwest::header::HeaderValue::from_str("application/json").unwrap(),
                        );
                    } else if let Some(img) = request.query.get("image") {
                        request_builder = request_builder.body(img.clone());
                    }
                    let response = tokio::time::timeout(
                        std::time::Duration::from_secs(15),
                        request_builder.send(),
                    )
                    .await
                    .context("DdddOCR request timed out")??
                    .error_for_status()
                    .context("DdddOCR server error")?;
                    let status = response.status().as_u16();
                    let headers = response
                        .headers()
                        .iter()
                        .map(|(n, v)| {
                            (
                                n.as_str().to_string(),
                                v.to_str().unwrap_or_default().to_string(),
                            )
                        })
                        .collect();
                    let body = response.bytes().await?.to_vec();
                    Ok(PluginResponse {
                        status,
                        headers,
                        body,
                    })
                }
                "urldecode" => {
                    // QD 兼容（qd web/handlers/util.py UrlDecodeHandler）：content 参数
                    // 已在 URL 解析层完成一次百分号解码（执行器会把 POST 表单体并入查询串），
                    // 这里直接按 QD 相同的缩进 JSON 返回，供
                    // success_asserts 的 "\"状态\": \"200\"" 与 extract_variables 的
                    // "\"转换后\": \"(.*)\"" 规则按原样匹配。
                    let content = request
                        .query
                        .get("content")
                        .map(String::as_str)
                        .unwrap_or("");
                    let value = serde_json::to_string(content)?;
                    let mut headers = BTreeMap::new();
                    headers.insert(
                        "content-type".to_string(),
                        "application/json; charset=UTF-8".to_string(),
                    );
                    Ok(PluginResponse {
                        status: 200,
                        headers,
                        body: format!("{{\n    \"转换后\": {value},\n    \"状态\": \"200\"\n}}")
                            .into_bytes(),
                    })
                }
                "gb2312" => {
                    // QD 兼容（qd web/handlers/util.py GB2312Handler）：把 content
                    // 按 GB2312 编码后逐字节百分号编码（urllib.parse.quote 语义），
                    // 返回与 QD 相同的缩进 JSON，供 success_asserts 的
                    // "\"状态\": \"200\"" 与 extract_variables 匹配。
                    let content = request
                        .query
                        .get("content")
                        .map(String::as_str)
                        .unwrap_or("");
                    let (gb_bytes, _, _) = encoding_rs::GBK.encode(content);
                    let encoded =
                        percent_encoding::percent_encode(&gb_bytes, GB2312_QUOTE_SET).to_string();
                    let value = serde_json::to_string(&encoded)?;
                    let mut headers = BTreeMap::new();
                    headers.insert(
                        "content-type".to_string(),
                        "application/json; charset=UTF-8".to_string(),
                    );
                    Ok(PluginResponse {
                        status: 200,
                        headers,
                        body: format!("{{\n    \"转换后\": {value},\n    \"状态\": \"200\"\n}}")
                            .into_bytes(),
                    })
                }
                "rsa" => {
                    // QD 兼容（qd web/handlers/util.py UtilRSAHandler）：key 支持
                    // PKCS#1/PKCS#8 公钥或私钥 PEM；f=encode 用公钥做 PKCS1 v1.5
                    // 加密并输出 Base64，f=decode 用私钥解 Base64 密文。键体中的
                    // 空格按 QD 的方式还原为 '+'（URL 传输丢失的加号）。
                    use rsa::pkcs1::DecodeRsaPrivateKey;
                    use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
                    let key = request.query.get("key").context("rsa key is required")?;
                    let data = request.query.get("data").context("rsa data is required")?;
                    let operation = request
                        .query
                        .get("f")
                        .map(String::as_str)
                        .unwrap_or("encode");
                    let pem = normalize_rsa_pem(key)?;
                    let private_key = rsa::RsaPrivateKey::from_pkcs1_pem(&pem)
                        .or_else(|_| rsa::RsaPrivateKey::from_pkcs8_pem(&pem))
                        .ok();
                    let body = match operation {
                        f if f.contains("encode") => {
                            let public_key = match private_key.as_ref() {
                                Some(private) => rsa::RsaPublicKey::from(private),
                                None => rsa::RsaPublicKey::from_public_key_pem(&pem).context(
                                    "证书格式错误: expected a PEM public or private key",
                                )?,
                            };
                            let mut rng = rand::thread_rng();
                            let encrypted = public_key
                                .encrypt(&mut rng, rsa::pkcs1v15::Pkcs1v15Encrypt, data.as_bytes())
                                .context("rsa encryption failed")?;
                            base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                encrypted,
                            )
                        }
                        f if f.contains("decode") => {
                            let private =
                                private_key.context("rsa decode requires a PEM private key")?;
                            let ciphertext = base64::Engine::decode(
                                &base64::engine::general_purpose::STANDARD,
                                data,
                            )
                            .context("invalid base64 ciphertext")?;
                            let decrypted = private
                                .decrypt(rsa::pkcs1v15::Pkcs1v15Encrypt, &ciphertext)
                                .context("rsa decryption failed")?;
                            String::from_utf8(decrypted)
                                .context("decrypted rsa data is not valid UTF-8")?
                        }
                        _ => bail!("功能选择错误: {operation}"),
                    };
                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: body.into_bytes(),
                    })
                }
                "string/replace" => {
                    // QD 兼容（qd web/handlers/util.py UtilStrReplaceHandler）：s
                    // 原文，p 正则，t 替换串（\1 形式的组引用自动翻译为 Rust
                    // regex 的 $1）。r=text 返回 HTML 转义纯文本，否则返回 QD
                    // 同款缩进 JSON。
                    let source = request.query.get("s").map(String::as_str).unwrap_or("");
                    let pattern = request
                        .query
                        .get("p")
                        .context("regex pattern p is required")?;
                    let replacement = request.query.get("t").map(String::as_str).unwrap_or("");
                    let re = regex::Regex::new(pattern).context("invalid regex pattern")?;
                    let processed = re
                        .replace_all(source, translate_python_replacement(replacement))
                        .to_string();
                    let body = if request.query.get("r").map(String::as_str) == Some("text") {
                        html_escape::encode_text(&processed).to_string()
                    } else {
                        let s_json = serde_json::to_string(source)?;
                        let t_json = serde_json::to_string(&processed)?;
                        format!(
                            "{{\n    \"原始字符串\": {s_json},\n    \"处理后字符串\": {t_json},\n    \"状态\": \"OK\"\n}}"
                        )
                    };
                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: body.into_bytes(),
                    })
                }
                // QD 兼容（qd web/handlers/util.py ToolboxNotepadHandler）：模板的
                // 跨运行状态。槽位是「账号 + id_notepad」两元组，值是一整块文本，
                // read / write / append 三个动作，而且**不管读写都把当前值当响应体
                // 返回** —— 模板正是靠这一点把刷新后的 cookie 接回下一次运行。
                //
                // 与 QD 只有两处差别，都记在 docs/reference.md：账号取自这次运行
                // 而不是 URL 里的 email/pwd（模板里不该出现账号密码），以及写入有上限。
                "toolbox/notepad" => {
                    notepad_slot_action(self.notepad.as_deref(), request, false).await
                }
                "toolbox/notepad/list" => {
                    notepad_slot_action(self.notepad.as_deref(), request, true).await
                }
                // QD also registers `toolbox/<digits>/notepad[/<digits>]`. Its own
                // POST handler ignores those path segments — it re-reads
                // `id_notepad` from the query and takes the account from email/pwd
                // — so there is nothing to port beyond saying which spelling
                // works. Without this arm the generic message below would read as
                // if the whole namespace were missing.
                action if action.starts_with("toolbox/") => bail!(
                    "unsupported toolbox route: util/{action} \
                     (util/toolbox/notepad and util/toolbox/notepad/list are the ported \
                     spellings; pass the slot as id_notepad in the query)"
                ),
                action => bail!("plugin action unavailable: util/{action}"),
            }
        })
    }
}

/// `api://util/toolbox/notepad` and `api://util/toolbox/notepad/list`, ported
/// from QD's `ToolboxNotepadHandler` and `ToolboxNotepadListHandler`.
///
/// Failures answer as a **400 response carrying QD's own message**, not as an
/// executor error. QD catches its exception, writes the text and sets 400, so a
/// template receives a response and its own assertions decide what that means;
/// bailing would abort the step instead, which is a different outcome for the
/// same template.
async fn notepad_slot_action(
    notepad: Option<&dyn NotepadStore>,
    request: &PluginRequest,
    list: bool,
) -> Result<PluginResponse> {
    let Some(notepad) = notepad else {
        // Not a template error: this run has no storage behind it. The CLI has
        // no database, and saying so beats "action unavailable", which would
        // read as if the route were never ported.
        bail!(
            "plugin action unavailable: util/{} \
             (no notepad storage is attached to this run)",
            request.action
        );
    };
    // QD reads every argument the same way whether it arrives as a query or as a
    // form body, and `merge_form_into_query` has already merged the two before
    // the call reaches here — so `data=` can be a POST body, which is what QD's
    // own HAR editor emits.
    let arg = |name: &str| request.query.get(name).map(String::as_str);

    let flag = arg("f").unwrap_or(if list { "list" } else { "" });
    if flag.is_empty() {
        // The two routes spell this message differently in QD, so the two
        // spellings are kept: which route a template hit is what decides it.
        return Ok(notepad_error(if list {
            NOTEPAD_INCOMPLETE_LIST
        } else {
            NOTEPAD_INCOMPLETE
        }));
    }

    // The slot comes from the query on both routes, which is what QD's own POST
    // does even when the path form carries one. The defaults differ between the
    // two routes in QD: the notepad route defaults to slot 1, the list route to
    // "unassigned", which QD spells -1.
    let raw_slot = arg("id_notepad").unwrap_or(if list { "" } else { "1" });
    let slot = if list && raw_slot.is_empty() {
        -1
    } else {
        match raw_slot.parse::<i64>() {
            Ok(slot) => slot,
            Err(_) => {
                // QD's `int(...)` raises here and it writes that exception's
                // text, which is a Python artifact rather than a contract; this
                // says the same thing in a form worth reading.
                return Ok(notepad_error(&format!(
                    "id_notepad 不是一个整数: {raw_slot}"
                )));
            }
        }
    };

    if list {
        return notepad_list_action(notepad, request, flag, slot).await;
    }

    let stored = match notepad.read(slot).await? {
        // QD creates slot 1 on first use — the slot a template gets without
        // asking — and refuses every other missing slot.
        NotepadSlot::Missing if slot == 1 => {
            notepad.write(slot, None).await?;
            None
        }
        NotepadSlot::Missing => return Ok(notepad_error("记事本不存在")),
        NotepadSlot::Stored(content) => content,
    };

    let data = arg("data").unwrap_or("");
    // QD tests `write` first and both by substring, so `f=write...` writes and
    // anything that is not an append — `f=read`, or a flag it does not know —
    // reads. The order is the behaviour, not an accident.
    let writing = flag.contains("write");
    let appending = !writing && flag.contains("append");
    let body = if writing {
        data.to_string()
    } else if appending {
        // CRLF, and only when something is already stored: a slot QD just
        // created holds NULL, not "", and appending to it does not start with a
        // blank line.
        match &stored {
            Some(existing) => format!("{existing}\r\n{data}"),
            None => data.to_string(),
        }
    } else {
        stored.unwrap_or_default()
    };
    if writing || appending {
        if body.len() > NOTEPAD_CONTENT_LIMIT {
            return Ok(notepad_error(&format!(
                "记事本内容超过上限, limit: {NOTEPAD_CONTENT_LIMIT}"
            )));
        }
        notepad.write(slot, Some(body.clone())).await?;
    }
    // The body is the value *after* the operation, which is the whole point of
    // the contract: the template extracts the refreshed cookie from the same
    // step that stored it.
    Ok(notepad_text(&body))
}

/// The management route. QD also drives this from a page this repo does not
/// have, so here it is the only way to create a slot above 1.
async fn notepad_list_action(
    notepad: &dyn NotepadStore,
    request: &PluginRequest,
    flag: &str,
    slot: i64,
) -> Result<PluginResponse> {
    let mut slots = notepad.slots().await?;
    slots.sort_unstable();
    if slots.is_empty() {
        return Ok(notepad_error("无法获取该用户记事本编号"));
    }
    if flag.contains("add") {
        if slots.len() >= NOTEPAD_LIMIT {
            return Ok(notepad_error(&format!(
                "记事本数量超过上限, limit: {NOTEPAD_LIMIT}"
            )));
        }
        // QD turns an absent-or-empty `data` into NULL rather than "".
        let content = match request.query.get("data").map(String::as_str) {
            None | Some("") => None,
            Some(data) => Some(data.to_string()),
        };
        let target = if slot == -1 {
            slots.last().copied().unwrap_or(0) + 1
        } else if slots.contains(&slot) {
            return Ok(notepad_error(&format!(
                "记事本编号已存在, id_notepad: {slot}"
            )));
        } else {
            slot
        };
        notepad.write(target, content).await?;
        return Ok(notepad_text(&format!("添加成功, id_notepad: {target}")));
    }
    if flag.contains("delete") {
        if slot <= 0 {
            return Ok(notepad_error("id_notepad参数不完整, 请确认"));
        }
        if !slots.contains(&slot) {
            return Ok(notepad_error(&format!(
                "记事本编号不存在, id_notepad: {slot}"
            )));
        }
        if slot == 1 {
            return Ok(notepad_error("默认记事本不能删除"));
        }
        notepad.remove(slot).await?;
        return Ok(notepad_text(&format!("删除成功, id_notepad: {slot}")));
    }
    if flag.contains("list") {
        // QD writes the Python list itself, so the body is `[1, 2]`, and a
        // template that parses this body is parsing that.
        let rendered = slots
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(notepad_text(&format!("[{rendered}]")));
    }
    Ok(notepad_error(NOTEPAD_INCOMPLETE_LIST))
}

fn notepad_text(body: &str) -> PluginResponse {
    PluginResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: body.as_bytes().to_vec(),
    }
}

fn notepad_error(message: &str) -> PluginResponse {
    PluginResponse {
        status: 400,
        headers: BTreeMap::new(),
        body: message.as_bytes().to_vec(),
    }
}

fn valid_plugin_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Percent-encode set matching Python `urllib.parse.quote` defaults: ASCII
/// letters, digits, `_.-~` and `/` stay literal, everything else (including
/// all non-ASCII bytes) is percent-encoded.
const GB2312_QUOTE_SET: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// Python distutils strtobool semantics used by QD util handlers.
fn strtobool(value: &str) -> bool {
    matches!(
        value.to_lowercase().as_str(),
        "y" | "yes" | "t" | "true" | "on" | "1"
    )
}

/// QD's `delay_max_timeout`. qd-today reads it from its config; the same 300
/// seconds is the ceiling this build enforces.
const DELAY_MAX_SECONDS: f64 = 300.0;

/// Python's `str(float)` for the magnitudes a delay carries: `0.0`, `3.0`,
/// `1.5`. Rust's `{}` drops the `.0` an integral float needs to read as one.
fn qd_float(value: f64) -> String {
    let text = format!("{value}");
    if text
        .bytes()
        .any(|byte| !byte.is_ascii_digit() && byte != b'-')
    {
        text
    } else {
        format!("{text}.0")
    }
}

/// A JSON document that keeps object keys in insertion order, the way a Python
/// `dict` does.
///
/// `serde_json::Map` is a `BTreeMap` in this build, so the `{"1": …, "2": …,
/// "10": …}` QD's util handlers write would come out reordered. QD produces
/// those bodies with `json.dumps(..., ensure_ascii=False, indent=4)` and
/// templates read them back with `"…": "(.*)"` rules, so both the order and
/// the four-space layout are part of what a template matches on.
enum QdJson {
    Value(serde_json::Value),
    Object(Vec<(String, QdJson)>),
}

impl QdJson {
    fn object(entries: Vec<(String, QdJson)>) -> Self {
        Self::Object(entries)
    }

    fn value(value: serde_json::Value) -> Self {
        Self::Value(value)
    }
}

impl Serialize for QdJson {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;

        match self {
            Self::Value(value) => value.serialize(serializer),
            Self::Object(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

/// A QD util handler's response body: `json.dumps(..., ensure_ascii=False,
/// indent=4)`.
fn qd_json(value: &QdJson) -> Result<String> {
    let mut buffer = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut buffer, formatter);
    value.serialize(&mut serializer)?;
    String::from_utf8(buffer).context("QD JSON body is not valid UTF-8")
}

/// The `{"状态": ...}` object every QD util handler answers with when the work
/// itself raised, in place of its result keys.
fn qd_status(message: &str) -> QdJson {
    QdJson::object(vec![(
        "状态".to_string(),
        QdJson::value(serde_json::json!(message)),
    )])
}

/// The `strftime` directives Python's `time` module and chrono agree on.
///
/// chrono's formatter *panics* on a directive it does not recognise, where
/// Python raises `ValueError: Invalid format string` — so a format string is
/// checked against this set before it reaches the formatter, and an unknown
/// directive becomes the same `状态` text Python would produce. Directives only
/// one of the two knows (`%q`, `%s`, the `%.3f` family) are left out rather
/// than guessed at, because reading one as the other would print a different
/// date.
const PYTHON_STRFTIME_DIRECTIVES: &str = "aAbBcdeEfGHIjMmpSuUVwWxXyYzZ%";

/// `TimeStampHandler`'s time source: `dt` (read with `form`) wins over `ts`,
/// and both absent means "now".
fn timestamp_input(
    ts_arg: &str,
    dt_arg: &str,
    format: &str,
) -> std::result::Result<Option<f64>, String> {
    if !dt_arg.is_empty() {
        return match chrono::NaiveDateTime::parse_from_str(dt_arg, format) {
            Ok(naive) => match naive.and_local_timezone(chrono::Local).single() {
                Some(local) => Ok(Some(local.timestamp_micros() as f64 / 1_000_000.0)),
                None => Err(format!("ambiguous local time: {dt_arg:?}")),
            },
            Err(err) => Err(format!(
                "time data {dt_arg:?} does not match format {format:?}: {err}"
            )),
        };
    }
    if ts_arg.is_empty() {
        return Ok(None);
    }
    ts_arg
        .parse::<f64>()
        .map(Some)
        .map_err(|_| format!("could not convert string to float: {ts_arg:?}"))
}

/// `strftime` over the directives both Python and chrono read (see
/// [`PYTHON_STRFTIME_DIRECTIVES`]).
fn python_strftime<Tz: chrono::TimeZone>(
    time: &chrono::DateTime<Tz>,
    format: &str,
) -> std::result::Result<String, String>
where
    Tz::Offset: std::fmt::Display,
{
    let mut characters = format.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            continue;
        }
        match characters.next() {
            Some(directive) if PYTHON_STRFTIME_DIRECTIVES.contains(directive) => {}
            _ => return Err(format!("Invalid format string: {format:?}")),
        }
    }
    Ok(time.format(format).to_string())
}

/// Python's `datetime.isoformat()`: the fractional part is omitted when the
/// microsecond is zero, and QD replaces the `+00:00` offset with `Z`.
fn iso_format(utc: &chrono::DateTime<chrono::Utc>) -> String {
    use chrono::Timelike as _;

    let mut rendered = utc.format("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = utc.nanosecond() / 1_000;
    if micros != 0 {
        rendered.push_str(&format!(".{micros:06}"));
    }
    rendered.push('Z');
    rendered
}

/// QD's `yearday()`: how many days the year has, as a string.
fn yearday(year: i32) -> String {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    if leap { "366" } else { "365" }.to_string()
}

/// The `api://util/timestamp` body, shaped like `TimeStampHandler`.
///
/// The quirks are QD's and are kept: `dt` overrides `ts`, a `ts` of exactly
/// zero counts as absent (Python's `if ts:`), the `本机时间` key appears only in
/// that fallback branch, and any failure is answered as `{"状态": …}` rather
/// than failing the step.
fn timestamp_body(ts_arg: &str, dt_arg: &str, format: &str) -> Result<String> {
    use chrono::{DateTime, FixedOffset, Local, Utc};

    let given = match timestamp_input(ts_arg, dt_arg, format) {
        Ok(given) => given,
        Err(message) => return qd_json(&qd_status(&message)),
    };
    let current = given.is_none_or(|seconds| seconds == 0.0);
    let micros = match given.filter(|seconds| *seconds != 0.0) {
        Some(seconds) => (seconds * 1_000_000.0).round() as i64,
        None => Utc::now().timestamp_micros(),
    };
    let Some(utc) = DateTime::from_timestamp_micros(micros) else {
        return qd_json(&qd_status(&format!(
            "timestamp out of range: {micros} microseconds"
        )));
    };

    // China standard time is a fixed +08:00, so no time zone database is
    // needed for the 北京时间 key even on a host with none installed.
    let cst = FixedOffset::east_opt(8 * 3600).expect("UTC+8 is a valid offset");
    let local = utc.with_timezone(&Local);
    let beijing = utc.with_timezone(&cst);
    let complete = micros as f64 / 1_000_000.0;

    let mut entries: Vec<(&str, QdJson)> = vec![
        ("完整时间戳", QdJson::value(serde_json::json!(complete))),
        (
            "时间戳",
            QdJson::value(serde_json::json!(complete.trunc() as i64)),
        ),
        (
            "16位时间戳",
            QdJson::value(serde_json::json!((complete * 1_000_000.0) as i64)),
        ),
    ];
    let rendered = (|| -> std::result::Result<Vec<(&str, String)>, String> {
        use chrono::Datelike as _;

        let mut rendered = Vec::new();
        if current {
            rendered.push(("本机时间", python_strftime(&local, format)?));
        }
        rendered.push(("周", python_strftime(&local, "%w/%W")?));
        rendered.push((
            "日",
            format!(
                "{}/{}",
                python_strftime(&local, "%j")?,
                yearday(local.year())
            ),
        ));
        rendered.push(("北京时间", python_strftime(&beijing, format)?));
        rendered.push((
            "GMT格式",
            python_strftime(&utc, "%a, %d %b %Y %H:%M:%S GMT")?,
        ));
        rendered.push(("ISO格式", iso_format(&utc)));
        Ok(rendered)
    })();
    match rendered {
        Ok(rendered) => entries.extend(
            rendered
                .into_iter()
                .map(|(key, value)| (key, QdJson::value(serde_json::json!(value)))),
        ),
        Err(message) => return qd_json(&qd_status(&message)),
    }
    entries.push(("状态", QdJson::value(serde_json::json!("200"))));
    qd_json(&QdJson::object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    ))
}

/// Normalize an RSA PEM key the way QD does: locate the `-----BEGIN/END...-----`
/// markers even when all newlines were stripped by URL transport, restore '+'
/// characters that turned into spaces, and re-wrap the base64 body at 64
/// columns so PKCS#1/PKCS#8 PEM parsers accept it regardless of formatting.
fn normalize_rsa_pem(key: &str) -> Result<String> {
    let header_re = regex::Regex::new(r"-----BEGIN [^-]+-----").expect("valid header regex");
    let footer_re = regex::Regex::new(r"-----END [^-]+-----").expect("valid footer regex");
    let text = key.trim();
    let header = header_re
        .find(text)
        .map(|m| m.as_str().to_string())
        .context("证书格式错误: PEM header/footer is missing")?;
    let stripped = header_re.replace(text, "");
    let footer = footer_re
        .find(&stripped)
        .map(|m| m.as_str().to_string())
        .context("证书格式错误: PEM header/footer is missing")?;
    let body_source = footer_re.replace(&stripped, "").to_string();
    let body: String = body_source
        .replace(' ', "+")
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    ensure!(!body.is_empty(), "证书格式错误: PEM body is missing");
    let mut pem = format!("{header}\n");
    for chunk in body.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).context("key body is not valid UTF-8")?);
        pem.push('\n');
    }
    pem.push_str(&footer);
    pem.push('\n');
    Ok(pem)
}

/// Translate Python `re.sub` replacement syntax (`\1` group references,
/// `\\` literal backslash) into the Rust regex replacement syntax (`$1`,
/// `\`) used by `Regex::replace_all`.
fn translate_python_replacement(replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len());
    let mut chars = replacement.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some(digit @ '1'..='9') => {
                out.push('$');
                out.push(digit);
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::SocketAddr;

    use axum::{Router, routing::post};

    /// Answer one DdddOCR route, so that "the request actually arrived" can be
    /// asserted instead of inferred from the absence of an error.
    async fn serve_ddddocr() -> SocketAddr {
        let app = Router::new().route("/ocr", post(|| async { "recognized" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        address
    }

    async fn forward_to_ddddocr(
        policy: OutboundPolicy,
        address: SocketAddr,
    ) -> Result<PluginResponse> {
        let plugin = UtilityPlugin::with_policy(policy);
        let request =
            PluginRequest::from_api_url(&format!("api://util/dddd/ocr?_server=http://{address}/"))?;
        plugin.call(&request).await
    }

    /// `dddd/*` is the only `util` action that leaves this process, and its
    /// target comes from the template through `_server`. It used to build its
    /// own client, so a template could aim it at the loopback address the guard
    /// exists to refuse while the switches said no — one hole in an otherwise
    /// uniform gate, and one a template author could reach on purpose.
    ///
    /// Both directions are pinned. The allowed one asserts the response body:
    /// a guard that refused everything would pass a test that only looked for
    /// the refusal.
    #[tokio::test]
    async fn a_dddd_forward_goes_through_the_shared_guard() {
        let address = serve_ddddocr().await;

        let refused = forward_to_ddddocr(OutboundPolicy::default(), address)
            .await
            .unwrap_err();
        assert!(
            format!("{refused:#}").contains("private or special-use network target is blocked"),
            "a template must not reach loopback through this forward while the switch is off: \
             {refused:#}"
        );

        let opened = OutboundPolicy {
            allow_private_network: true,
            ..OutboundPolicy::default()
        };
        let response = forward_to_ddddocr(opened, address).await.unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"recognized");
    }

    /// The guard above is only as good as its exclusivity: a client built here
    /// would route around it, and `_server` is template-controlled, so that is
    /// a live SSRF rather than a hypothetical.
    ///
    /// Structural, and it reads the production half only — a needle that also
    /// matched this assertion's own wording would never fail and would assert
    /// nothing.
    #[test]
    fn no_bare_client_in_this_module() {
        let source = include_str!("plugin.rs").replace("\r\n", "\n");
        // Cut at the test module, not at the first `#[cfg(test)]`. This file
        // carries a `#[cfg(test)]` accessor inside `SubprocessPlugin` near the
        // top, so the first-marker rule ended the "production" half at line
        // 147 — above the DdddOCR branch this assertion exists to protect, and
        // a mutation that restored a bare client there was reported MISSED
        // rather than caught. An assertion whose range is decided by whichever
        // marker happens to come first stops covering the code it names.
        let production = source
            .split("\nmod tests {")
            .next()
            .expect("the file is never empty");
        let needle = ["reqwest::", "Client"].concat();
        assert!(
            !production.contains(&needle),
            "plugin.rs builds its own client; the DdddOCR forward goes through \
             guarded_client_for_url"
        );
    }

    fn echo_manifest() -> PluginManifest {
        PluginManifest {
            api_version: PLUGIN_API_VERSION,
            id: "echo".into(),
            name: "Echo".into(),
            version: "1.0.0".into(),
            capabilities: Vec::new(),
        }
    }

    #[test]
    fn capability_as_str_matches_wire_names() {
        assert_eq!(PluginCapability::Network.as_str(), "network");
        assert_eq!(PluginCapability::ReadFile.as_str(), "read_file");
        assert_eq!(PluginCapability::WriteFile.as_str(), "write_file");
        assert_eq!(PluginCapability::Environment.as_str(), "environment");
    }

    #[test]
    fn response_envelope_without_capabilities_stays_compatible() {
        // Plugins written before capability reporting reply with a bare
        // PluginResponse; the envelope must still deserialize (wire format is
        // additive, api_version stays 1).
        let envelope: PluginCallResponse =
            serde_json::from_str(r#"{"status":200,"headers":{},"body":[104,105]}"#).unwrap();
        assert!(envelope.capabilities_used.is_empty());
        assert_eq!(envelope.response.body, b"hi");
    }

    #[test]
    fn undeclared_capability_is_rejected_with_plugin_context() {
        let mut manifest = echo_manifest();
        manifest.capabilities = vec![PluginCapability::ReadFile];
        let plugin = SubprocessPlugin::from_command(manifest, "echo-plugin").unwrap();
        let request = PluginRequest {
            plugin_id: "echo".into(),
            action: "fetch".into(),
            query: BTreeMap::new(),
        };

        let error = plugin
            .ensure_declared_capabilities(&request, &[PluginCapability::Network])
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("plugin echo/fetch used undeclared capability: network"),
            "{message}"
        );
        assert!(message.contains("declared: read_file"), "{message}");

        // A declared capability passes untouched.
        plugin
            .ensure_declared_capabilities(&request, &[PluginCapability::ReadFile])
            .unwrap();
    }

    #[test]
    fn from_command_splits_program_and_arguments() {
        let plugin =
            SubprocessPlugin::from_command(echo_manifest(), "  /opt/bin/echo   --flag 1  ")
                .unwrap();
        let (program, arguments) = plugin.command_parts();
        assert_eq!(program, Path::new("/opt/bin/echo"));
        assert_eq!(
            arguments.to_vec(),
            vec!["--flag".to_string(), "1".to_string()]
        );

        // A bare executable stays the program with no arguments, i.e. today's
        // behaviour for commands stored before arguments were supported.
        let bare = SubprocessPlugin::from_command(echo_manifest(), "echo-plugin").unwrap();
        let (program, arguments) = bare.command_parts();
        assert_eq!(program, Path::new("echo-plugin"));
        assert!(arguments.is_empty());
    }

    #[test]
    fn from_command_rejects_blank_command() {
        assert!(
            SubprocessPlugin::from_command(echo_manifest(), "   ")
                .unwrap_err()
                .to_string()
                .contains("plugin command is empty")
        );
    }

    #[tokio::test]
    async fn failure_names_plugin_id_action_and_registry() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        assert_eq!(registry.ids(), vec!["util".to_string()]);

        let error = registry
            .call("api://mock/echo", Duration::from_secs(1))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("plugin unavailable: mock/echo"), "{error}");
        assert!(error.contains("registered: util"), "{error}");

        // An action the plugin does not implement keeps the id/action prefix.
        let error = registry
            .call("api://util/nope", Duration::from_secs(1))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("plugin call failed: util/nope"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn times_out_with_plugin_id_and_action() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        let error = registry
            .call("api://util/delay?seconds=1", Duration::from_millis(10))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("plugin call timed out: util/delay"),
            "{error}"
        );
    }

    #[test]
    fn parses_qd_api_url() {
        let request = PluginRequest::from_api_url("api://util/delay?seconds=1.5").unwrap();
        assert_eq!(request.plugin_id, "util");
        assert_eq!(request.action, "delay");
        assert_eq!(
            request.query.get("seconds").map(String::as_str),
            Some("1.5")
        );
    }

    #[test]
    fn parses_qd_delay_path_form() {
        // The spelling QD's own HAR editor inserts: its "insert delay" button
        // writes `api://util/delay/3` (see qiandao
        // web/static/har/entry_editor.js), backed by the
        // `/util/delay/(\d+)` and `/util/delay/(\d+\.\d+)` routes.
        let request = PluginRequest::from_api_url("api://util/delay/3").unwrap();
        assert_eq!(request.plugin_id, "util");
        assert_eq!(request.action, "delay/3");
        assert!(request.query.is_empty());

        let float = PluginRequest::from_api_url("api://util/delay/3.5").unwrap();
        assert_eq!(float.action, "delay/3.5");
    }

    #[tokio::test]
    async fn registers_and_calls_builtin_utility() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        let response = registry
            .call("api://util/delay?seconds=0", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"delay 0.0 second.");
    }

    #[tokio::test]
    async fn calls_delay_through_qd_path_form() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        let response = registry
            .call("api://util/delay/0", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"delay 0.0 second.");
    }

    #[tokio::test]
    async fn rejects_missing_plugin_and_excessive_delay() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        assert!(
            registry
                .call("api://missing/action", Duration::from_secs(1))
                .await
                .unwrap_err()
                .to_string()
                .contains("plugin unavailable")
        );
        assert!(
            registry
                .call("api://util/delay?seconds=301", Duration::from_secs(1))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn delay_answers_with_qd_text_and_clamps_negative_seconds() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        let response = registry
            .call("api://util/delay?seconds=1.5", Duration::from_secs(3))
            .await
            .unwrap();
        assert_eq!(response.body, b"delay 1.5 second.");
        // QD clamps a negative delay to zero and still sleeps zero seconds.
        let negative = registry
            .call("api://util/delay/-5", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(negative.body, b"delay 0.0 second.");
        // An unreadable value is a reply, not a failed step.
        let broken = registry
            .call("api://util/delay/soon", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(broken.body, b"Error, delay 0.0 second.");
    }

    #[tokio::test]
    async fn regex_reports_the_qd_data_table() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        let response = registry
            .call(
                "api://util/regex?data=a1b2c3&p=%5Cd%2B",
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(response.body).unwrap(),
            "{\n    \"数据\": {\n        \"1\": \"1\",\n        \"2\": \"2\",\n        \"3\": \"3\"\n    },\n    \"状态\": \"OK\"\n}"
        );

        // `re.findall` shapes an entry by the pattern's group count, and QD
        // compiles with `re.IGNORECASE` — so the uppercase `A` is a match the
        // pattern `[a-z]` alone would not report.
        let grouped = registry
            .call(
                "api://util/regex?data=A1&p=([a-z])(%5Cd)",
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(grouped.body).unwrap(),
            "{\n    \"数据\": {\n        \"1\": [\n            \"A\",\n            \"1\"\n        ]\n    },\n    \"状态\": \"OK\"\n}"
        );

        // A pattern Python refuses is reported in 状态, without a 数据 key —
        // the step still succeeds, exactly as QD's except branch does.
        let broken = registry
            .call(
                "api://util/regex?data=a&p=(unclosed",
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        let body = String::from_utf8(broken.body).unwrap();
        assert!(body.starts_with("{\n    \"状态\": "), "{body}");
        assert!(!body.contains("\"数据\""), "{body}");
    }

    #[tokio::test]
    async fn timestamp_reports_the_qd_key_set() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        let response = registry
            .call("api://util/timestamp?ts=1700000000", Duration::from_secs(1))
            .await
            .unwrap();
        let body = String::from_utf8(response.body).unwrap();
        // The order of the keys is QD's too.
        for expected in [
            "\"完整时间戳\": 1700000000.0",
            "\"时间戳\": 1700000000",
            "\"16位时间戳\": 1700000000000000",
            "\"北京时间\": \"2023-11-15 06:13:20\"",
            "\"GMT格式\": \"Tue, 14 Nov 2023 22:13:20 GMT\"",
            "\"ISO格式\": \"2023-11-14T22:13:20Z\"",
            "\"状态\": \"200\"",
        ] {
            assert!(body.contains(expected), "{expected} missing from {body}");
        }
        // 本机时间 belongs to the "no timestamp given" branch only.
        assert!(body.contains("\"周\":"));
        assert!(!body.contains("\"本机时间\":"));
        assert!(
            body.find("\"完整时间戳\"").unwrap() < body.find("\"状态\"").unwrap(),
            "{body}"
        );

        // Without `ts`/`dt` the body describes the host clock and carries the
        // extra 本机时间 key.
        let now = registry
            .call("api://util/timestamp", Duration::from_secs(1))
            .await
            .unwrap();
        let now = String::from_utf8(now.body).unwrap();
        assert!(now.contains("\"本机时间\":"));
        assert!(now.contains("\"状态\": \"200\""));

        // An unreadable timestamp is reported in 状态, as QD's except does.
        let broken = registry
            .call("api://util/timestamp?ts=soon", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(broken.body).unwrap(),
            "{\n    \"状态\": \"could not convert string to float: \\\"soon\\\"\"\n}"
        );
    }

    #[tokio::test]
    async fn urldecode_returns_qd_compatible_body() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        let response = registry
            .call(
                "api://util/urldecode?content=%E7%AD%BE%E5%88%B0%E6%88%90%E5%8A%9F%EF%BC%9A%E8%8E%B7%E5%BE%9720%E7%A7%AF%E5%88%86",
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        let body = String::from_utf8(response.body).unwrap();
        assert_eq!(
            body,
            "{\n    \"转换后\": \"签到成功：获得20积分\",\n    \"状态\": \"200\"\n}"
        );
    }

    #[tokio::test]
    async fn gb2312_encodes_content_with_qd_json_body() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        // "中文" in GBK bytes: D6 D0 CE C4.
        let response = registry
            .call(
                "api://util/gb2312?content=%E4%B8%AD%E6%96%87",
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        let body = String::from_utf8(response.body).unwrap();
        assert!(
            body.contains("\"转换后\": \"%D6%D0%CE%C4\""),
            "unexpected body: {body}"
        );
        assert!(body.contains("\"状态\": \"200\""));
    }

    #[tokio::test]
    async fn unicode_converts_content_with_qd_json_body() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        // 189天翼云 flow: content is urlencoded ASCII hex - passes through and
        // lands in the "转换后" field for the "\"转换后\": \"(.*)\"" extractor.
        let response = registry
            .call("api://util/unicode?content=ab%20cd", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        let body = String::from_utf8(response.body).unwrap();
        assert!(
            body.contains("\"转换后\": \"ab cd\""),
            "unexpected body: {body}"
        );
        assert!(body.contains("\"状态\": \"200\""));

        // Embedded \uXXXX escapes are decoded, matching QD's conver2unicode.
        let response = registry
            .call(
                "api://util/unicode?content=%5Cu79ef%5Cu5206",
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        let body = String::from_utf8(response.body).unwrap();
        assert!(
            body.contains("\"转换后\": \"积分\""),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn string_replace_supports_python_group_references() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        // s="hello world", p="(world)", t="\1!" (percent-encoded).
        let response = registry
            .call(
                "api://util/string/replace?s=hello%20world&p=(world)&t=%5C1%21",
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        let body = String::from_utf8(response.body).unwrap();
        assert!(
            body.contains("\"处理后字符串\": \"hello world!\""),
            "unexpected body: {body}"
        );
        assert!(body.contains("\"状态\": \"OK\""));

        // r=text returns the HTML-escaped result directly, like QD.
        // s="acb", p="b", t="<b>&amp;" -> processed "ac<b>&amp;".
        let response = registry
            .call(
                "api://util/string/replace?s=acb&p=b&t=%3Cb%3E%26amp%3B&r=text",
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(response.body).unwrap(),
            "ac&lt;b&gt;&amp;amp;"
        );
    }

    #[tokio::test]
    async fn computes_totp_locally() {
        let mut registry = PluginRegistry::default();
        registry
            .register(Arc::new(UtilityPlugin::default()))
            .unwrap();
        // RFC 6238 vector: 8 digits, sha1, t=59.
        let response = registry
            .call(
                "api://util/totp?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&digits=8&t=59",
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(String::from_utf8(response.body).unwrap(), "94287082");

        // A bad secret or digit count is refused rather than answered with a
        // wrong code.
        assert!(
            registry
                .call("api://util/totp?secret=nope!", Duration::from_secs(1))
                .await
                .is_err()
        );
        assert!(
            registry
                .call(
                    "api://util/totp?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&digits=4",
                    Duration::from_secs(1),
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn rsa_encodes_and_decodes_roundtrip() {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        use rsa::pkcs8::EncodePublicKey;

        let mut rng = rand::thread_rng();
        let private = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public = rsa::RsaPublicKey::from(&private);
        let private_pem = (*private.to_pkcs1_pem(rsa::pkcs8::LineEnding::LF).unwrap()).clone();
        let public_pem = public
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        // Simulate a key mangled by URL transport: newlines stripped and '+'
        // turned into spaces; normalize_rsa_pem must restore it.
        let flattened = public_pem.replace('\n', "").replace('+', " ");

        let plugin = UtilityPlugin::default();
        let request = PluginRequest {
            plugin_id: "util".into(),
            action: "rsa".into(),
            query: [
                ("key".to_string(), flattened),
                ("data".to_string(), "签到 secret 123".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let response = plugin.call(&request).await.unwrap();
        assert_eq!(response.status, 200);
        let encrypted = String::from_utf8(response.body).unwrap();
        assert!(!encrypted.is_empty());

        let request = PluginRequest {
            plugin_id: "util".into(),
            action: "rsa".into(),
            query: [
                ("key".to_string(), private_pem),
                ("data".to_string(), encrypted),
                ("f".to_string(), "decode".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let response = plugin.call(&request).await.unwrap();
        assert_eq!(String::from_utf8(response.body).unwrap(), "签到 secret 123");
    }

    // ---- The notepad: the one `util` route that keeps state across runs ----

    /// An in-memory notepad for the tests that only care about the route.
    /// `None` content is kept as `None`: the route tells it apart from `""`.
    #[derive(Debug, Default)]
    struct MockNotepad {
        slots: std::sync::Mutex<BTreeMap<i64, Option<String>>>,
    }

    impl MockNotepad {
        fn snapshot(&self) -> BTreeMap<i64, Option<String>> {
            self.slots.lock().unwrap().clone()
        }
    }

    impl NotepadStore for MockNotepad {
        fn read<'a>(
            &'a self,
            slot: i64,
        ) -> Pin<Box<dyn Future<Output = Result<NotepadSlot>> + Send + 'a>> {
            Box::pin(async move {
                let content = { self.slots.lock().unwrap().get(&slot).cloned() };
                Ok(match content {
                    None => NotepadSlot::Missing,
                    Some(content) => NotepadSlot::Stored(content),
                })
            })
        }

        fn write<'a>(
            &'a self,
            slot: i64,
            content: Option<String>,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            Box::pin(async move {
                self.slots.lock().unwrap().insert(slot, content);
                Ok(())
            })
        }

        fn slots<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<Vec<i64>>> + Send + 'a>> {
            Box::pin(async move {
                let slots: Vec<i64> = self.slots.lock().unwrap().keys().copied().collect();
                Ok(slots)
            })
        }

        fn remove<'a>(
            &'a self,
            slot: i64,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            Box::pin(async move {
                self.slots.lock().unwrap().remove(&slot);
                Ok(())
            })
        }
    }

    /// One notepad call. The status comes back with the body because QD reports
    /// a notepad failure as a 400 *response* — a template sees it and its own
    /// assertions decide what it means.
    async fn notepad_call(notepad: Option<Arc<MockNotepad>>, url: &str) -> (u16, String) {
        let handle = notepad.map(|notepad| notepad as Arc<dyn NotepadStore>);
        let plugin = UtilityPlugin::default().with_notepad(handle);
        let request = PluginRequest::from_api_url(url).unwrap();
        let response = plugin.call(&request).await.unwrap();
        (response.status, String::from_utf8(response.body).unwrap())
    }

    /// What a QD template actually relies on: the body is the value *after* the
    /// operation, so the step that stores a refreshed cookie is also the step
    /// that hands it back for `extract_variables`.
    #[tokio::test]
    async fn the_notepad_returns_the_stored_value_from_every_operation() {
        let notepad = Arc::new(MockNotepad::default());

        // Reading slot 1 before anything wrote it creates it, empty — QD's own
        // auto-create, and the reason its message is not 「记事本不存在」.
        let (status, body) =
            notepad_call(Some(notepad.clone()), "api://util/toolbox/notepad?f=read").await;
        assert_eq!((status, body.as_str()), (200, ""));
        assert_eq!(notepad.snapshot(), BTreeMap::from([(1, None)]));

        let (status, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad?f=write&data=cookie%3Da",
        )
        .await;
        assert_eq!((status, body.as_str()), (200, "cookie=a"));

        let (status, body) =
            notepad_call(Some(notepad.clone()), "api://util/toolbox/notepad?f=read").await;
        assert_eq!((status, body.as_str()), (200, "cookie=a"));

        // Append joins with a CRLF and returns the joined value.
        let (status, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad?f=append&data=b",
        )
        .await;
        assert_eq!((status, body.as_str()), (200, "cookie=a\r\nb"));
        assert_eq!(
            notepad.snapshot().get(&1),
            Some(&Some("cookie=a\r\nb".to_string()))
        );
    }

    /// The one place NULL and `""` differ: a slot QD created itself starts
    /// appending without a leading blank line, and a slot written empty does not.
    #[tokio::test]
    async fn appending_to_a_fresh_slot_does_not_start_with_a_blank_line() {
        let notepad = Arc::new(MockNotepad::default());

        let (_, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad?f=append&data=first",
        )
        .await;
        assert_eq!(body, "first");
        let (_, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad?f=append&data=second",
        )
        .await;
        assert_eq!(body, "first\r\nsecond");

        // An explicit empty write leaves `""`, not NULL, so the separator is
        // there on the next append.
        notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad?f=write&data=",
        )
        .await;
        let (_, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad?f=append&data=x",
        )
        .await;
        assert_eq!(body, "\r\nx");
    }

    /// QD tests `write` first, and by substring. `f=writeappend` therefore
    /// writes; the order is the behaviour, not an accident.
    #[tokio::test]
    async fn write_wins_when_the_flag_says_both() {
        let notepad = Arc::new(MockNotepad::default());
        notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad?f=write&data=first",
        )
        .await;
        let (_, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad?f=writeappend&data=second",
        )
        .await;
        assert_eq!(body, "second");
    }

    /// QD creates slot 1 on demand and refuses every other missing slot, with
    /// that message and as a response rather than a dead run.
    #[tokio::test]
    async fn only_slot_one_is_created_on_demand() {
        let notepad = Arc::new(MockNotepad::default());
        let (status, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad?id_notepad=2&f=read",
        )
        .await;
        assert_eq!((status, body.as_str()), (400, "记事本不存在"));
        assert!(notepad.snapshot().is_empty(), "the refusal created a slot");

        // `f` is required, and that is checked before the slot is.
        let (status, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad?id_notepad=3",
        )
        .await;
        assert_eq!((status, body.as_str()), (400, "参数不完整，请确认"));
    }

    /// QD authenticates this route with `email`/`pwd` arguments. This port takes
    /// the account from the run, so they are neither required nor read: a
    /// migrated template that carries them keeps working, one without them works
    /// too, and the password never has to sit inside a template.
    #[tokio::test]
    async fn the_account_never_comes_from_the_url() {
        let notepad = Arc::new(MockNotepad::default());
        let (status, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad?email=a%40b.c&pwd=hunter2&f=write&data=secret",
        )
        .await;
        assert_eq!((status, body.as_str()), (200, "secret"));
        assert_eq!(
            notepad.snapshot().get(&1),
            Some(&Some("secret".to_string()))
        );
        // Nothing from the URL but the stored value comes back: the response is
        // the body a template extracts into a variable, and a credential in it
        // would be a credential in the run log.
        assert!(!body.contains("hunter2"));
    }

    /// The management route, which is also the only way a *run* can create a slot
    /// above 1: the notepad page can make one for a person, but a template has
    /// only this route.
    #[tokio::test]
    async fn the_notepad_list_route_adds_and_deletes_slots() {
        let notepad = Arc::new(MockNotepad::default());

        // Nothing yet: QD cannot name a slot for an account that has none, and
        // this route cannot create the first one either — QD's notepad page does
        // that, and here the notepad route's own first read does.
        let (status, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad/list?f=list",
        )
        .await;
        assert_eq!((status, body.as_str()), (400, "无法获取该用户记事本编号"));
        let (status, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad/list?f=add&data=a",
        )
        .await;
        assert_eq!((status, body.as_str()), (400, "无法获取该用户记事本编号"));

        notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad?f=write&data=a",
        )
        .await;
        assert_eq!(notepad.snapshot(), BTreeMap::from([(1, Some("a".into()))]));
        // `f` defaults to `list` on this route, and add numbers the slot itself.
        let (_, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad/list?f=add&data=b",
        )
        .await;
        assert_eq!(body, "添加成功, id_notepad: 2");
        // QD writes its own Python list here, so the body is `[1, 2]`.
        let (_, body) =
            notepad_call(Some(notepad.clone()), "api://util/toolbox/notepad/list").await;
        assert_eq!(body, "[1, 2]");

        let (status, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad/list?f=add&id_notepad=2&data=c",
        )
        .await;
        assert_eq!(
            (status, body.as_str()),
            (400, "记事本编号已存在, id_notepad: 2")
        );

        // Slot 1 is what a template gets without asking, so it stays.
        let (status, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad/list?f=delete&id_notepad=1",
        )
        .await;
        assert_eq!((status, body.as_str()), (400, "默认记事本不能删除"));
        let (_, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad/list?f=delete&id_notepad=2",
        )
        .await;
        assert_eq!(body, "删除成功, id_notepad: 2");
        assert_eq!(
            notepad.snapshot().keys().copied().collect::<Vec<_>>(),
            vec![1]
        );
    }

    /// The slot cap is QD's `NOTEPAD_LIMIT`, enforced where QD enforces it: on
    /// the add, not on a read.
    #[tokio::test]
    async fn the_notepad_refuses_a_slot_past_the_limit() {
        let notepad = Arc::new(MockNotepad::default());
        // The list route cannot create the first slot, so the first read does.
        notepad_call(Some(notepad.clone()), "api://util/toolbox/notepad?f=read").await;
        for _ in 2..=NOTEPAD_LIMIT {
            let (status, body) = notepad_call(
                Some(notepad.clone()),
                "api://util/toolbox/notepad/list?f=add&data=x",
            )
            .await;
            assert_eq!(status, 200, "{body}");
        }
        let (status, body) = notepad_call(
            Some(notepad.clone()),
            "api://util/toolbox/notepad/list?f=add",
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(body, format!("记事本数量超过上限, limit: {NOTEPAD_LIMIT}"));
    }

    /// QD puts no bound on the stored text; this port does, so one template
    /// cannot fill the database through one slot.
    #[tokio::test]
    async fn the_notepad_refuses_content_past_its_limit() {
        let notepad = Arc::new(MockNotepad::default());
        let oversized = "x".repeat(NOTEPAD_CONTENT_LIMIT + 1);
        let (status, body) = notepad_call(
            Some(notepad.clone()),
            &format!("api://util/toolbox/notepad?f=write&data={oversized}"),
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(
            body,
            format!("记事本内容超过上限, limit: {NOTEPAD_CONTENT_LIMIT}")
        );
        // The slot QD creates on first use is still there — that happens before
        // the flag is read, which is QD's own order — but the oversized value is
        // not.
        assert_eq!(notepad.snapshot(), BTreeMap::from([(1, None)]));
    }

    /// A run whose executor was built without storage — the CLI, and any test
    /// that does not ask for one. Saying so beats "action unavailable", which
    /// would read as if the route had never been ported.
    #[tokio::test]
    async fn without_storage_the_notepad_says_nothing_is_attached() {
        let plugin = UtilityPlugin::default();
        let request = PluginRequest::from_api_url("api://util/toolbox/notepad?f=read").unwrap();
        let error = plugin.call(&request).await.unwrap_err().to_string();
        assert!(error.contains("no notepad storage is attached"), "{error}");
    }

    /// QD also registers `toolbox/<digits>/notepad[/<digits>]`. Its own POST
    /// ignores those path segments, so there is nothing to port beyond saying
    /// which spelling works.
    #[tokio::test]
    async fn the_per_user_path_spelling_says_which_one_to_use() {
        let plugin = UtilityPlugin::default();
        let request = PluginRequest::from_api_url("api://util/toolbox/1/notepad/2?f=read").unwrap();
        let error = plugin.call(&request).await.unwrap_err().to_string();
        assert!(error.contains("unsupported toolbox route"), "{error}");
        assert!(error.contains("util/toolbox/notepad"), "{error}");
    }
}
