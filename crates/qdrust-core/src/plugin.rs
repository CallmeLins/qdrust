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
}
