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
