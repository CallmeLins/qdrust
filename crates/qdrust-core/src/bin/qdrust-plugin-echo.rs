use std::io::{self, BufRead};

use qdrust_core::plugin::{PluginRequest, PluginResponse};

fn main() -> anyhow::Result<()> {
    let line = io::stdin()
        .lock()
        .lines()
        .next()
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("missing plugin request"))?;
    let request: PluginRequest = serde_json::from_str(&line)?;
    let response = PluginResponse {
        status: 200,
        headers: Default::default(),
        body: format!("{}:{}", request.plugin_id, request.action).into_bytes(),
    };
    serde_json::to_writer(io::stdout(), &response)?;
    Ok(())
}
