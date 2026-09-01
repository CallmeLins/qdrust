use std::io::{self, BufRead};

use qdrust_core::plugin::PluginRequest;

fn main() -> anyhow::Result<()> {
    let line = io::stdin()
        .lock()
        .lines()
        .next()
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("missing plugin request"))?;
    let request: PluginRequest = serde_json::from_str(&line)?;
    // `?capability=<name>` makes the fixture report that capability so tests
    // can exercise the host-side "used must be declared" enforcement. The
    // name is passed through verbatim: recognized names deserialize into the
    // capability enum, unrecognized ones make the whole envelope an invalid
    // plugin response on the host side.
    let capabilities_used: Vec<String> = request
        .query
        .get("capability")
        .into_iter()
        .cloned()
        .collect();
    let envelope = serde_json::json!({
        "status": 200,
        "headers": {},
        "body": format!("{}:{}", request.plugin_id, request.action).into_bytes(),
        "capabilities_used": capabilities_used,
    });
    serde_json::to_writer(io::stdout(), &envelope)?;
    Ok(())
}
