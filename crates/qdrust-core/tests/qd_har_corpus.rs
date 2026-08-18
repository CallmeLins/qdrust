use std::{fs, path::PathBuf};

use anyhow::{Context, Result, ensure};
use qdrust_core::qd_har::{QdHar, QdProgram};
use serde_json::Value;

#[test]
fn parses_and_preserves_every_qd_har_fixture() -> Result<()> {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut paths = fs::read_dir(&fixtures)
        .with_context(|| format!("cannot read fixture directory {}", fixtures.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "har"))
        .collect::<Vec<_>>();
    paths.sort();
    ensure!(!paths.is_empty(), "QD HAR fixture corpus is empty");

    for path in paths {
        let bytes = fs::read(&path)
            .with_context(|| format!("cannot read QD HAR fixture {}", path.display()))?;
        let raw: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("fixture is not JSON: {}", path.display()))?;
        let har = QdHar::parse(raw.clone())
            .with_context(|| format!("fixture is not compatible: {}", path.display()))?;
        ensure!(har.raw() == &raw, "fixture changed during import");
        QdProgram::compile(&har)
            .with_context(|| format!("fixture control flow is invalid: {}", path.display()))?;
    }

    Ok(())
}
