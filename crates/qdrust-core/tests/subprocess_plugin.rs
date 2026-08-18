use std::{sync::Arc, time::Duration};

use qdrust_core::plugin::{PLUGIN_API_VERSION, PluginManifest, PluginRegistry, SubprocessPlugin};

#[tokio::test]
async fn calls_json_lines_subprocess_plugin() {
    let manifest = PluginManifest {
        api_version: PLUGIN_API_VERSION,
        id: "echo".into(),
        name: "Echo fixture".into(),
        version: "1.0.0".into(),
        capabilities: Vec::new(),
    };
    let plugin = SubprocessPlugin::new(
        manifest,
        env!("CARGO_BIN_EXE_qdrust-plugin-echo"),
        Vec::new(),
    )
    .unwrap();
    let mut registry = PluginRegistry::default();
    registry.register(Arc::new(plugin)).unwrap();

    let response = registry
        .call("api://echo/ping", Duration::from_secs(5))
        .await
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"echo:ping");
}
