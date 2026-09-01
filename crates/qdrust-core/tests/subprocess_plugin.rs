use std::{sync::Arc, time::Duration};

use qdrust_core::plugin::{
    PLUGIN_API_VERSION, PluginCapability, PluginManifest, PluginRegistry, SubprocessPlugin,
};

fn manifest(capabilities: Vec<PluginCapability>) -> PluginManifest {
    PluginManifest {
        api_version: PLUGIN_API_VERSION,
        id: "echo".into(),
        name: "Echo fixture".into(),
        version: "1.0.0".into(),
        capabilities,
    }
}

fn registry(plugin: SubprocessPlugin) -> PluginRegistry {
    let mut registry = PluginRegistry::default();
    registry.register(Arc::new(plugin)).unwrap();
    registry
}

#[tokio::test]
async fn calls_json_lines_subprocess_plugin() {
    let plugin = SubprocessPlugin::new(
        manifest(Vec::new()),
        env!("CARGO_BIN_EXE_qdrust-plugin-echo"),
        Vec::new(),
    )
    .unwrap();

    let response = registry(plugin)
        .call("api://echo/ping", Duration::from_secs(5))
        .await
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"echo:ping");
}

#[tokio::test]
async fn declared_capability_report_is_accepted() {
    // The fixture reports `network` for this call and the manifest declares
    // it, so the call succeeds.
    let plugin = SubprocessPlugin::new(
        manifest(vec![PluginCapability::Network]),
        env!("CARGO_BIN_EXE_qdrust-plugin-echo"),
        Vec::new(),
    )
    .unwrap();

    let response = registry(plugin)
        .call(
            "api://echo/fetch?capability=network",
            Duration::from_secs(5),
        )
        .await
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"echo:fetch");
}

#[tokio::test]
async fn undeclared_capability_report_rejects_the_call() {
    // The fixture reports `network` but the manifest declares nothing:
    // ADR-0006 requires the host to reject the call, not silently accept it.
    let plugin = SubprocessPlugin::new(
        manifest(Vec::new()),
        env!("CARGO_BIN_EXE_qdrust-plugin-echo"),
        Vec::new(),
    )
    .unwrap();

    let error = registry(plugin)
        .call(
            "api://echo/fetch?capability=network",
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("plugin echo/fetch used undeclared capability: network"),
        "{message}"
    );
    assert!(message.contains("declared: <none>"), "{message}");
}

#[tokio::test]
async fn report_of_unknown_capability_name_is_an_invalid_response() {
    // A malformed report must fail closed: the envelope cannot even be
    // deserialized, so the call is rejected as an invalid plugin response.
    let plugin = SubprocessPlugin::new(
        manifest(vec![PluginCapability::Network]),
        env!("CARGO_BIN_EXE_qdrust-plugin-echo"),
        Vec::new(),
    )
    .unwrap();

    let error = registry(plugin)
        .call("api://echo/fetch?capability=root", Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("invalid plugin response"),
        "{error:#}"
    );
}
