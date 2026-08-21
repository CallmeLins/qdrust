use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

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
            serde_json::from_slice(&output.stdout).context("invalid plugin response")
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

    pub async fn call(&self, value: &str, timeout: Duration) -> Result<PluginResponse> {
        let request = PluginRequest::from_api_url(value)?;
        let plugin = self
            .plugins
            .get(&request.plugin_id)
            .with_context(|| format!("plugin unavailable: {}", request.plugin_id))?;
        tokio::time::timeout(timeout, plugin.call(&request))
            .await
            .context("plugin call timed out")?
    }
}

pub struct UtilityPlugin {
    manifest: PluginManifest,
}

impl Default for UtilityPlugin {
    fn default() -> Self {
        Self {
            manifest: PluginManifest {
                api_version: PLUGIN_API_VERSION,
                id: "util".into(),
                name: "Built-in utilities".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                capabilities: Vec::new(),
            },
        }
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
                "delay" => {
                    let seconds = request
                        .query
                        .get("seconds")
                        .map(String::as_str)
                        .unwrap_or("0")
                        .parse::<f64>()
                        .context("invalid delay seconds")?;
                    ensure!(
                        seconds.is_finite() && (0.0..=300.0).contains(&seconds),
                        "delay must be between 0 and 300 seconds"
                    );
                    tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: format!("delayed {seconds} seconds").into_bytes(),
                    })
                }
                "timestamp" => {
                    let format = request.query.get("format").map(String::as_str);
                    let timestamp = chrono::Utc::now().timestamp();
                    let body = match format {
                        Some("ms") => (timestamp * 1000).to_string(),
                        _ => timestamp.to_string(),
                    };
                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: body.into_bytes(),
                    })
                }
                "unicode" => {
                    let text = request.query.get("text").map(String::as_str).unwrap_or("");
                    let decoded = percent_encoding::percent_decode_str(text)
                        .decode_utf8()
                        .context("invalid unicode encoding")?;
                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: decoded.into_owned().into_bytes(),
                    })
                }
                "regex" => {
                    let pattern = request
                        .query
                        .get("pattern")
                        .context("regex pattern is required")?;
                    let text = request.query.get("text").map(String::as_str).unwrap_or("");
                    let operation = request
                        .query
                        .get("op")
                        .map(String::as_str)
                        .unwrap_or("search");

                    let re = regex::Regex::new(pattern).context("invalid regex pattern")?;

                    let result = match operation {
                        "search" => re.find(text).map(|m| m.as_str()).unwrap_or("").to_string(),
                        "findall" => {
                            let matches: Vec<&str> =
                                re.find_iter(text).map(|m| m.as_str()).collect();
                            serde_json::to_string(&matches)?
                        }
                        "replace" => {
                            let replacement = request
                                .query
                                .get("replacement")
                                .map(String::as_str)
                                .unwrap_or("");
                            re.replace_all(text, replacement).to_string()
                        }
                        _ => bail!("unsupported regex operation: {operation}"),
                    };

                    Ok(PluginResponse {
                        status: 200,
                        headers: BTreeMap::new(),
                        body: result.into_bytes(),
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
                    let client = reqwest::Client::new();
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
                action => bail!("plugin action unavailable: util/{action}"),
            }
        })
    }
}

fn valid_plugin_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(response.body, b"delayed 0 seconds");
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
}
