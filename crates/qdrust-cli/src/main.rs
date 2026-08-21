use std::{env, fs};

use anyhow::{Context, Result, bail};
use qdrust_core::qd_har::{QdHar, QdProgram};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("validate") => {
            let path = args
                .next()
                .context("usage: qdrust-cli validate <file.har.json>")?;
            let source =
                fs::read_to_string(&path).with_context(|| format!("cannot read {path}"))?;
            let har: serde_json::Value = serde_json::from_str(&source).context("invalid JSON")?;
            let parsed = QdHar::parse(har).context("invalid QD HAR")?;
            QdProgram::compile(&parsed).context("HAR compilation failed")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "valid": true,
                    "entries": parsed.entries().len(),
                    "enabled": parsed.enabled_entries().count()
                }))?
            );
            Ok(())
        }
        Some("run") => {
            let path = args
                .next()
                .context("usage: qdrust-cli run <file.har.json>")?;
            let mut variables = std::collections::BTreeMap::new();
            let mut timeout = 300_u64;
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--var" => {
                        let value = args.next().context("--var requires key=value")?;
                        let (key, raw) =
                            value.split_once('=').context("--var requires key=value")?;
                        anyhow::ensure!(!key.is_empty(), "variable name cannot be empty");
                        let parsed = serde_json::from_str(raw)
                            .unwrap_or_else(|_| serde_json::Value::String(raw.to_owned()));
                        variables.insert(key.to_owned(), parsed);
                    }
                    "--timeout" => {
                        timeout = args
                            .next()
                            .context("--timeout requires seconds")?
                            .parse()
                            .context("invalid timeout")?
                    }
                    other => bail!("unknown run option: {other}"),
                }
            }
            let source =
                fs::read_to_string(&path).with_context(|| format!("cannot read {path}"))?;
            let har: serde_json::Value = serde_json::from_str(&source).context("invalid JSON")?;
            let parsed = QdHar::parse(har).context("invalid QD HAR")?;
            let program = QdProgram::compile(&parsed).context("HAR compilation failed")?;
            let executor =
                qdrust_core::executor::QdExecutor::new(std::time::Duration::from_secs(30))?;
            let mut context = qdrust_core::executor::ExecutionContext::new(variables);
            let steps = executor
                .execute_with_deadline(
                    &program,
                    &mut context,
                    std::time::Duration::from_secs(timeout),
                )
                .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"ok": true, "steps": steps}))?
            );
            Ok(())
        }
        Some("backup") => {
            let path = args
                .next()
                .context("usage: qdrust-cli backup <database.sqlite> [output]")?;
            let output = args.next().unwrap_or_else(|| {
                let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                format!("{path}.backup-{stamp}")
            });
            backup_sqlite(&path, &output)?;
            println!("backup written to {output}");
            Ok(())
        }
        Some("restore") => {
            let path = args
                .next()
                .context("usage: qdrust-cli restore <database.sqlite> <backup-file>")?;
            let backup = args
                .next()
                .context("usage: qdrust-cli restore <database.sqlite> <backup-file>")?;
            fs::copy(&backup, &path)
                .with_context(|| format!("cannot restore {backup} into {path}"))?;
            println!("restored {backup} into {path}");
            println!("note: stop the server before restoring a SQLite database");
            Ok(())
        }
        Some("help") | None => {
            println!("qdrust-cli validate <file.har.json>");
            println!("qdrust-cli run <file.har.json> [--var key=value] [--timeout seconds]");
            println!("qdrust-cli backup <database.sqlite> [output]");
            println!("qdrust-cli restore <database.sqlite> <backup-file>");
            Ok(())
        }
        Some(command) => bail!("unknown command: {command}"),
    }
}

/// Online SQLite backup using the VACUUM INTO command (SQLite 3.27+).
fn backup_sqlite(path: &str, output: &str) -> Result<()> {
    let conn =
        rusqlite::Connection::open(path).with_context(|| format!("cannot open database {path}"))?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    let escaped = output.replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{escaped}';"))
        .with_context(|| format!("cannot vacuum into {output}"))?;
    Ok(())
}
