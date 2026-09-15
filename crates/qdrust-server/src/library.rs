//! Browsing a subscription source and importing only the entries a user picks.
//!
//! A source is usually a public QD template library. The official one
//! (<https://github.com/qd-today/templates>) publishes `tpls_history.json` at
//! its root: a manifest keyed by template name whose values carry the author,
//! the variable documentation, the upstream `yyyymmdd` version, and either a
//! base64-encoded HAR (`content`) or the file to download (`filename`).
//!
//! Sources without a manifest still work: the repository tree is scanned and
//! every QD template file becomes an entry, just without the extra metadata.
//!
//! Discovery lives here rather than in `subscriptions` so the "import
//! everything" sync and the "import what I picked" library share one
//! catalogue and one import path — a template imported either way ends up with
//! the same provenance row and updates in place.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result, ensure};
use base64::Engine;
use qdrust_core::qd_har::QdHar;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;

use crate::model::{
    ImportQdHarTemplate, LibraryEntry, LibraryImportFailure, LibraryImportOutcome,
    LibraryImportResult, TemplateImport, TemplateLibrary, TemplateSubscription,
    UpdateQdHarTemplate,
};
use crate::store::Store;

/// Upper bound on how many entries one source may contribute, so a repository
/// with a runaway tree (or a hostile manifest) cannot blow up memory or a
/// response. Comfortably above the several hundred the official library ships.
const MAX_LIBRARY_ENTRIES: usize = 2000;

/// The manifest file a library publishes to describe its templates. Part of the
/// QD third-party-library contract (https://github.com/qd-today/templates).
const MANIFEST_FILE: &str = "tpls_history.json";

const USER_AGENT: &str = "qdrust-subscription";

/// A GitHub repository, as referenced by a subscription URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GitHubSource {
    pub owner: String,
    pub repo: String,
    pub branch: String,
}

impl GitHubSource {
    fn raw_url(&self, path: &str) -> String {
        format!(
            "https://raw.githubusercontent.com/{}/{}/{}/{}",
            self.owner, self.repo, self.branch, path
        )
    }

    fn blob_url(&self, path: &str) -> String {
        format!(
            "https://github.com/{}/{}/blob/{}/{}",
            self.owner, self.repo, self.branch, path
        )
    }
}

/// Parse a GitHub repository URL into its parts. Accepts a plain repo URL and
/// the `/tree/<branch>` / `/blob/<branch>` forms GitHub's UI produces; a path
/// prefix after the branch is ignored because the whole tree is scanned.
pub(crate) fn parse_github_url(url: &str) -> Option<GitHubSource> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();
    let owner = parts.first()?.to_string();
    let repo = parts.get(1)?.trim_end_matches(".git").to_string();
    parts.drain(0..2);
    let mut branch = "HEAD".to_string();
    if let Some(first) = parts.first().copied()
        && (first == "tree" || first == "blob")
    {
        parts.remove(0);
        if let Some(named) = parts.first().copied() {
            branch = named.to_string();
            parts.remove(0);
        }
    }
    Some(GitHubSource {
        owner,
        repo,
        branch,
    })
}

/// One template a source offers, before its HAR is fetched.
#[derive(Clone, Debug)]
pub(crate) struct RawEntry {
    /// Identity inside the source: the manifest `har` key, or the file path for
    /// a scanned repository. Also the local template name.
    pub(crate) name: String,
    pub(crate) author: Option<String>,
    pub(crate) comments: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) date: Option<String>,
    pub(crate) filename: String,
    pub(crate) url: Option<String>,
    pub(crate) comment_url: Option<String>,
    /// Base64-encoded HAR, when the manifest inlines it. Preferred over `url`
    /// because it is the exact revision the manifest describes and needs no
    /// second request.
    pub(crate) content: Option<String>,
}

/// A source's catalogue plus how it was obtained.
pub(crate) struct Catalogue {
    /// `manifest` or `files`.
    pub(crate) source_kind: &'static str,
    pub(crate) manifest_version: Option<String>,
    pub(crate) entries: Vec<RawEntry>,
}

impl Catalogue {
    pub(crate) fn by_name(&self) -> HashMap<&str, &RawEntry> {
        self.entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry))
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    har: BTreeMap<String, ManifestEntry>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ManifestEntry {
    // The record's own `name` is not read: the manifest requires it to equal
    // its key, and the key is what stays stable across renames upstream.
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    comments: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    commenturl: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

/// List one source's templates, marking the ones already imported and the ones
/// whose upstream version has moved on.
pub async fn browse(
    store: &Store,
    client: &Client,
    subscription: &TemplateSubscription,
) -> Result<TemplateLibrary> {
    let catalogue = catalogue(client, &subscription.url).await?;
    let installed = installed_index(store.list_template_imports(subscription.id).await?);
    let entries = catalogue
        .entries
        .iter()
        .map(|entry| library_entry(entry, installed.get(&entry.name)))
        .collect();
    Ok(TemplateLibrary {
        subscription_id: subscription.id,
        source_kind: catalogue.source_kind.to_string(),
        manifest_version: catalogue.manifest_version.clone(),
        entries,
    })
}

/// Import the named entries, or refresh them when they are already linked.
/// Entries are reported individually so one broken upstream template does not
/// discard the rest of the selection.
pub async fn import_selected(
    store: &Store,
    client: &Client,
    subscription: &TemplateSubscription,
    names: &[String],
) -> Result<LibraryImportResult> {
    ensure!(!names.is_empty(), "no templates selected");
    let catalogue = catalogue(client, &subscription.url).await?;
    let index = catalogue.by_name();
    let linked = installed_index(store.list_template_imports(subscription.id).await?);
    let source = parse_github_url(&subscription.url);
    let mut result = LibraryImportResult {
        imported: 0,
        updated: 0,
        failed: Vec::new(),
        templates: Vec::new(),
    };
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name.as_str()) {
            continue;
        }
        let Some(entry) = index.get(name.as_str()) else {
            result.failed.push(LibraryImportFailure {
                name: name.clone(),
                error: "this source does not offer that template".to_string(),
            });
            continue;
        };
        match import_entry(
            store,
            client,
            subscription,
            source.as_ref(),
            entry,
            linked
                .get(entry.name.as_str())
                .map(|import| import.template_id),
        )
        .await
        {
            Ok((template_id, updated)) => {
                if updated {
                    result.updated += 1;
                } else {
                    result.imported += 1;
                }
                result.templates.push(LibraryImportOutcome {
                    name: entry.name.clone(),
                    template_id,
                    updated,
                });
            }
            Err(err) => result.failed.push(LibraryImportFailure {
                name: name.clone(),
                error: bounded(&err.to_string()),
            }),
        }
    }
    Ok(result)
}

/// Import (or refresh) one entry and record where it came from.
/// Returns the template's id and whether an existing row was updated in place.
pub(crate) async fn import_entry(
    store: &Store,
    client: &Client,
    subscription: &TemplateSubscription,
    source: Option<&GitHubSource>,
    entry: &RawEntry,
    linked_template_id: Option<i64>,
) -> Result<(i64, bool)> {
    let har = resolve_har(client, source, entry).await?;
    QdHar::parse_qd(har.clone())
        .with_context(|| format!("{} is not a valid QD HAR template", entry.name))?;
    let description = entry.comments.as_deref().map(plain_text);
    let owner_id = subscription.owner_id;
    // Prefer the recorded provenance. Fall back to matching by name so
    // templates imported before provenance existed are refreshed in place
    // rather than duplicated — the same rule the automatic sync always used.
    let existing = match linked_template_id {
        Some(id) => Some(id),
        None => store.find_template_by_name(owner_id, &entry.name).await?,
    };
    let (template_id, updated) = match existing {
        Some(id) => {
            let template = store
                .update_qd_har_for_owner(
                    id,
                    owner_id,
                    UpdateQdHarTemplate {
                        name: entry.name.clone(),
                        description,
                        har,
                    },
                )
                .await?
                .with_context(|| format!("template {id} disappeared while importing"))?;
            (template.id, true)
        }
        None => {
            let template = store
                .import_qd_har_for_owner(
                    owner_id,
                    ImportQdHarTemplate {
                        name: entry.name.clone(),
                        description,
                        har,
                    },
                )
                .await?;
            (template.id, false)
        }
    };
    store
        .record_template_import(
            subscription.id,
            template_id,
            &entry.name,
            entry.version.as_deref(),
            entry.url.as_deref(),
        )
        .await?;
    Ok((template_id, updated))
}

/// Resolve an entry's HAR document, preferring the inlined base64 `content`
/// and otherwise downloading it.
async fn resolve_har(
    client: &Client,
    source: Option<&GitHubSource>,
    entry: &RawEntry,
) -> Result<Value> {
    if let Some(content) = entry.content.as_deref()
        && !content.trim().is_empty()
    {
        let bytes = decode_base64(content)
            .with_context(|| format!("{} has unreadable content", entry.name))?;
        return serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not valid JSON", entry.name));
    }
    let url = entry
        .url
        .clone()
        .or_else(|| source.map(|source| source.raw_url(&entry.filename)))
        .with_context(|| format!("{} has neither content nor a download URL", entry.name))?;
    let text = fetch_text(client, &url)
        .await?
        .with_context(|| format!("{} is not available at {url}", entry.name))?;
    serde_json::from_str(&text).with_context(|| format!("{url} is not valid JSON"))
}

/// Build a source's catalogue: the manifest when it publishes one, otherwise
/// the repository tree, otherwise a single direct file URL.
pub(crate) async fn catalogue(client: &Client, url: &str) -> Result<Catalogue> {
    if let Some(source) = parse_github_url(url) {
        if let Some(manifest) = fetch_manifest(client, &source).await? {
            if manifest.har.len() > MAX_LIBRARY_ENTRIES {
                // Surfaced rather than silently dropped: a truncated catalogue
                // would make the missing entries look like a search miss.
                tracing::warn!(
                    total = manifest.har.len(),
                    kept = MAX_LIBRARY_ENTRIES,
                    "source offers more templates than the library cap; listing only the first {}",
                    MAX_LIBRARY_ENTRIES
                );
            }
            let entries = manifest
                .har
                .iter()
                .take(MAX_LIBRARY_ENTRIES)
                .filter_map(|(key, entry)| manifest_raw_entry(key, entry))
                .collect::<Vec<_>>();
            ensure!(
                !entries.is_empty(),
                "{} lists no usable templates",
                MANIFEST_FILE
            );
            return Ok(Catalogue {
                source_kind: "manifest",
                manifest_version: manifest.version,
                entries,
            });
        }
        let entries = scan_tree(client, &source).await?;
        ensure!(
            !entries.is_empty(),
            "no template files found in that repository"
        );
        return Ok(Catalogue {
            source_kind: "files",
            manifest_version: None,
            entries,
        });
    }
    // A direct file URL is a one-entry source.
    let name = url
        .rsplit('/')
        .next()
        .unwrap_or("template")
        .trim_end_matches(".json")
        .trim_end_matches(".har")
        .to_string();
    Ok(Catalogue {
        source_kind: "files",
        manifest_version: None,
        entries: vec![RawEntry {
            filename: name.clone(),
            name,
            author: None,
            comments: None,
            version: None,
            date: None,
            url: Some(url.to_string()),
            comment_url: None,
            content: None,
        }],
    })
}

async fn fetch_manifest(client: &Client, source: &GitHubSource) -> Result<Option<Manifest>> {
    let url = source.raw_url(MANIFEST_FILE);
    let Some(text) = fetch_text(client, &url).await? else {
        return Ok(None);
    };
    let manifest: Manifest =
        serde_json::from_str(&text).with_context(|| format!("{url} is not a valid manifest"))?;
    Ok(Some(manifest))
}

/// Turn one manifest record into an entry, dropping records that carry no way
/// to obtain the HAR.
fn manifest_raw_entry(key: &str, entry: &ManifestEntry) -> Option<RawEntry> {
    let has_content = entry
        .content
        .as_deref()
        .is_some_and(|content| !content.trim().is_empty());
    let has_url = entry
        .url
        .as_deref()
        .is_some_and(|url| !url.trim().is_empty());
    let has_filename = entry
        .filename
        .as_deref()
        .is_some_and(|filename| !filename.trim().is_empty());
    // A record with no inlined HAR, no link and no filename cannot be fetched.
    if !has_content && !has_url && !has_filename {
        return None;
    }
    let filename = entry
        .filename
        .clone()
        .unwrap_or_else(|| format!("{key}.har"));
    Some(RawEntry {
        // The name is the manifest key: the contract requires it to be unique
        // and identical to the record's own `name`, and it is what the upstream
        // framework matches on.
        name: key.to_string(),
        author: entry
            .author
            .clone()
            .filter(|value| !value.trim().is_empty()),
        comments: entry
            .comments
            .clone()
            .filter(|value| !value.trim().is_empty()),
        version: entry
            .version
            .clone()
            .filter(|value| !value.trim().is_empty()),
        date: entry.date.clone().filter(|value| !value.trim().is_empty()),
        filename,
        url: entry.url.clone().filter(|value| !value.trim().is_empty()),
        comment_url: entry
            .commenturl
            .clone()
            .filter(|value| !value.trim().is_empty()),
        content: if has_content {
            entry.content.clone()
        } else {
            None
        },
    })
}

/// Scan a repository tree for QD template files, for sources that publish no
/// manifest. Entries carry no version, so they never report an update.
async fn scan_tree(client: &Client, source: &GitHubSource) -> Result<Vec<RawEntry>> {
    let api_url = format!(
        "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
        source.owner, source.repo, source.branch
    );
    let response = client
        .get(&api_url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("cannot reach GitHub API")?
        .error_for_status()
        .context("GitHub API returned an error")?;
    let body: Value = response
        .json()
        .await
        .context("invalid GitHub API response")?;
    let Some(tree) = body.get("tree").and_then(Value::as_array) else {
        anyhow::bail!("GitHub tree response has no entries");
    };
    let mut entries = Vec::new();
    for item in tree {
        if entries.len() >= MAX_LIBRARY_ENTRIES {
            break;
        }
        if item.get("type").and_then(Value::as_str) != Some("blob") {
            continue;
        }
        let Some(path) = item.get("path").and_then(Value::as_str) else {
            continue;
        };
        if !looks_like_qd_template(path) {
            continue;
        }
        entries.push(RawEntry {
            name: file_stem(path).to_string(),
            author: None,
            comments: None,
            version: None,
            date: None,
            filename: path.to_string(),
            url: Some(source.raw_url(path)),
            comment_url: Some(source.blob_url(path)),
            content: None,
        });
    }
    Ok(entries)
}

/// A QD template file. Anything else in the tree is a README, a licence or a
/// GitHub workflow, none of which parse as HAR.
fn looks_like_qd_template(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    (lower.ends_with(".har") || lower.ends_with(".json")) && !lower.ends_with(MANIFEST_FILE)
}

fn file_stem(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.trim_end_matches(".json").trim_end_matches(".har")
}

fn library_entry(entry: &RawEntry, installed: Option<&TemplateImport>) -> LibraryEntry {
    let installed_version = installed.and_then(|import| import.entry_version.clone());
    LibraryEntry {
        name: entry.name.clone(),
        author: entry.author.clone(),
        comments: entry.comments.as_deref().map(plain_text),
        version: entry.version.clone(),
        date: entry.date.clone(),
        filename: entry.filename.clone(),
        url: entry.url.clone(),
        comment_url: entry.comment_url.clone(),
        installed: installed.is_some(),
        installed_template_id: installed.map(|import| import.template_id),
        update_available: installed.is_some()
            && version_is_newer(entry.version.as_deref(), installed_version.as_deref()),
        installed_version,
    }
}

pub(crate) fn installed_index(imports: Vec<TemplateImport>) -> HashMap<String, TemplateImport> {
    imports
        .into_iter()
        .map(|import| (import.entry_name.clone(), import))
        .collect()
}

/// Whether `upstream` is a later revision than `installed`. QD versions are
/// `yyyymmdd` strings, so compare them numerically when both parse and fall
/// back to a string comparison otherwise. An unknown local version is treated
/// as up to date: without a baseline every entry would nag on every sync.
fn version_is_newer(upstream: Option<&str>, installed: Option<&str>) -> bool {
    let (Some(upstream), Some(installed)) = (upstream, installed) else {
        return false;
    };
    let (upstream, installed) = (upstream.trim(), installed.trim());
    if upstream.is_empty() || installed.is_empty() {
        return false;
    }
    match (upstream.parse::<u64>(), installed.parse::<u64>()) {
        (Ok(upstream), Ok(installed)) => upstream > installed,
        _ => upstream > installed,
    }
}

/// Decode the base64 the manifest embeds. QD writes standard base64, often
/// padded; tolerate stray whitespace and an unpadded tail.
fn decode_base64(value: &str) -> Result<Vec<u8>> {
    let compact = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let padded = base64::engine::general_purpose::STANDARD.decode(&compact);
    padded
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&compact))
        .context("template content is not valid base64")
}

/// Reduce the manifest's `comments` to plain text. The field is HTML-ish
/// (`账号密码签到<br>日志显示`) from an untrusted repository, so tags are
/// dropped here and the UI renders the result as text rather than injecting it.
fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '<' {
            out.push(character);
            continue;
        }
        let mut tag = String::new();
        for character in chars.by_ref() {
            if character == '>' {
                break;
            }
            tag.push(character);
        }
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(name.as_str(), "br" | "p" | "div" | "li") {
            out.push('\n');
        }
    }
    // Decode the entities a browser would, then collapse the newline runs block
    // tags produce into single breaks so a description reads as clean lines.
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn fetch_text(client: &Client, url: &str) -> Result<Option<String>> {
    let response = client
        .get(url)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .with_context(|| format!("cannot reach {url}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response
        .error_for_status()
        .with_context(|| format!("{url} returned an error"))?;
    Ok(Some(response.text().await.with_context(|| {
        format!("cannot read the body of {url}")
    })?))
}

fn bounded(message: &str) -> String {
    const MAX: usize = 4096;
    let mut bounded = message.chars().take(MAX).collect::<String>();
    if message.chars().count() > MAX {
        bounded.push_str("...");
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SubscriptionMode;
    use serde_json::json;

    /// The shape qd-today/templates publishes: a `har` map keyed by template
    /// name, values carrying metadata plus either a base64 `content` or a
    /// `filename` to download.
    const MANIFEST: &str = r#"{
        "version": "20230315",
        "har": {
            "雨晨分享站": {
                "name": "雨晨分享站",
                "author": "loveyanglove",
                "url": "https://raw.githubusercontent.com/qd-today/templates/master/雨晨分享站.har",
                "update": true,
                "comments": "账号密码签到<br>日志显示",
                "filename": "雨晨分享站.har",
                "content": "W3siYSI6MX1d",
                "date": "2026-04-25 10:00:03",
                "version": "20260425",
                "commenturl": "https://github.com/qd-today/templates/issues/780"
            },
            "S1论坛签到": {
                "name": "S1论坛签到",
                "author": "Antiky",
                "filename": "saraba1st.har",
                "date": "2023-01-11 20:30:00",
                "version": "20230112"
            },
            "坏记录": {
                "name": "坏记录",
                "author": "nobody",
                "date": "2020-01-01 00:00:00",
                "version": "20200101"
            }
        }
    }"#;

    fn parse_manifest(text: &str) -> Manifest {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn manifest_entries_carry_the_metadata_the_browser_shows() {
        let manifest = parse_manifest(MANIFEST);
        assert_eq!(manifest.version.as_deref(), Some("20230315"));
        assert_eq!(manifest.har.len(), 3);

        let entry = manifest_raw_entry("雨晨分享站", &manifest.har["雨晨分享站"]).unwrap();
        assert_eq!(entry.name, "雨晨分享站");
        assert_eq!(entry.author.as_deref(), Some("loveyanglove"));
        assert_eq!(entry.version.as_deref(), Some("20260425"));
        assert_eq!(entry.filename, "雨晨分享站.har");
        assert_eq!(
            entry.comment_url.as_deref(),
            Some("https://github.com/qd-today/templates/issues/780")
        );
        // The inlined HAR is kept for `resolve_har` to decode; `update` and the
        // record's own `name` are not part of the entry.
        assert_eq!(entry.content.as_deref(), Some("W3siYSI6MX1d"));
    }

    #[test]
    fn manifest_entry_without_a_filename_derives_one_from_its_key() {
        // Filename is required by the library contract, but an inlined HAR
        // makes it redundant, so it is filled in rather than the entry dropped.
        let manifest = parse_manifest(
            r#"{"har": {"fallback": {"name": "fallback", "content": "W3siYSI6MX1d"}}}"#,
        );
        let entry = manifest_raw_entry("fallback", &manifest.har["fallback"]).unwrap();
        assert_eq!(entry.filename, "fallback.har");
        assert_eq!(entry.content.as_deref(), Some("W3siYSI6MX1d"));

        let manifest = parse_manifest(
            r#"{"har": {"linked": {"name": "linked", "url": "https://example.com/linked.har"}}}"#,
        );
        let entry = manifest_raw_entry("linked", &manifest.har["linked"]).unwrap();
        assert_eq!(entry.filename, "linked.har");
        assert_eq!(entry.url.as_deref(), Some("https://example.com/linked.har"));
    }

    #[test]
    fn manifest_drops_records_that_offer_no_way_to_fetch_the_har() {
        // No filename, no url, no inlined HAR: nothing to download or decode.
        let manifest =
            parse_manifest(r#"{"har": {"empty": {"name": "empty", "version": "20240101"}}}"#);
        assert!(manifest_raw_entry("empty", &manifest.har["empty"]).is_none());
    }

    #[test]
    fn version_comparison_uses_the_numeric_form_when_it_parses() {
        assert!(version_is_newer(Some("20260425"), Some("20230112")));
        assert!(!version_is_newer(Some("20230112"), Some("20260425")));
        assert!(!version_is_newer(Some("20230112"), Some("20230112")));
        // No local baseline: an unknown version must not nag on every sync.
        assert!(!version_is_newer(Some("20260425"), None));
        assert!(!version_is_newer(None, Some("20230112")));
        assert!(!version_is_newer(Some("  "), Some("20230112")));
    }

    #[test]
    fn installed_entries_report_an_available_update() {
        let entry = RawEntry {
            name: "雨晨分享站".into(),
            author: None,
            comments: Some("账号密码签到<br>日志显示".into()),
            version: Some("20260425".into()),
            date: None,
            filename: "雨晨分享站.har".into(),
            url: None,
            comment_url: None,
            content: None,
        };
        let linked = TemplateImport {
            subscription_id: 7,
            template_id: 42,
            entry_name: "雨晨分享站".into(),
            entry_version: Some("20230112".into()),
        };
        let listed = library_entry(&entry, Some(&linked));
        assert!(listed.installed);
        assert_eq!(listed.installed_template_id, Some(42));
        assert!(listed.update_available);
        // The HTML-ish comments are reduced to text, never passed through.
        assert_eq!(listed.comments.as_deref(), Some("账号密码签到\n日志显示"));

        let not_installed = library_entry(&entry, None);
        assert!(!not_installed.installed);
        assert!(!not_installed.update_available);
        assert_eq!(not_installed.installed_template_id, None);
    }

    #[test]
    fn comments_are_reduced_to_plain_text() {
        assert_eq!(
            plain_text("账号密码签到<br>日志显示"),
            "账号密码签到\n日志显示"
        );
        assert_eq!(plain_text("<p>a</p><p>b</p>"), "a\nb");
        assert_eq!(plain_text("a<br><br>b"), "a\nb");
        assert_eq!(plain_text("  spaced  "), "spaced");
        assert_eq!(plain_text("a &amp; b &lt;c&gt;"), "a & b <c>");
        assert_eq!(plain_text("<br/>"), "");
        assert_eq!(plain_text(""), "");
    }

    #[test]
    fn base64_content_decodes_and_tolerates_whitespace() {
        assert_eq!(decode_base64("W3siYSI6MX1d").unwrap(), b"[{\"a\":1}]");
        assert_eq!(decode_base64("W3siYSI6MX1d\n").unwrap(), b"[{\"a\":1}]");
        assert!(decode_base64("not base64 !!").is_err());
    }

    #[test]
    fn github_urls_parse_into_owner_repo_and_branch() {
        let source = parse_github_url("https://github.com/qd-today/templates").unwrap();
        assert_eq!(source.owner, "qd-today");
        assert_eq!(source.repo, "templates");
        assert_eq!(source.branch, "HEAD");
        assert_eq!(
            source.raw_url("雨晨分享站.har"),
            "https://raw.githubusercontent.com/qd-today/templates/HEAD/雨晨分享站.har"
        );

        let branched =
            parse_github_url("https://github.com/qd-today/templates/tree/master/sub").unwrap();
        assert_eq!(branched.branch, "master");
        assert_eq!(
            parse_github_url("https://github.com/owner/repo.git")
                .unwrap()
                .repo,
            "repo"
        );
        assert!(parse_github_url("https://example.com/tpl.har").is_none());
    }

    #[test]
    fn tree_scan_only_accepts_template_files() {
        assert!(looks_like_qd_template("雨晨分享站.har"));
        assert!(looks_like_qd_template("sub/模板.json"));
        // The manifest itself and repository furniture are not templates.
        assert!(!looks_like_qd_template(MANIFEST_FILE));
        assert!(!looks_like_qd_template("README.md"));
        assert!(!looks_like_qd_template(".github/workflows/ci.yaml"));
        assert_eq!(file_stem("sub/雨晨分享站.har"), "雨晨分享站");
    }

    #[tokio::test]
    async fn catalogue_falls_back_to_a_single_entry_for_a_direct_file_url() {
        // A non-GitHub URL is a one-entry source; this branch reads nothing.
        let catalogue = catalogue(&Client::new(), "https://example.com/naive.har")
            .await
            .unwrap();
        assert_eq!(catalogue.source_kind, "files");
        assert_eq!(catalogue.entries.len(), 1);
        assert_eq!(catalogue.entries[0].name, "naive");
        assert_eq!(
            catalogue.entries[0].url.as_deref(),
            Some("https://example.com/naive.har")
        );
    }

    #[test]
    fn subscription_modes_parse_and_only_all_auto_syncs() {
        assert_eq!(
            SubscriptionMode::parse("select"),
            Some(SubscriptionMode::Select)
        );
        assert_eq!(SubscriptionMode::parse("ALL"), Some(SubscriptionMode::All));
        assert_eq!(SubscriptionMode::parse("sometimes"), None);
        assert!(SubscriptionMode::All.auto_syncs());
        assert!(!SubscriptionMode::Select.auto_syncs());
        assert_eq!(SubscriptionMode::Select.as_str(), "select");
    }

    #[test]
    fn json_shape_matches_what_the_browser_renders() {
        // Guards the field names the WebUI reads off `GET .../library`.
        let entry = library_entry(
            &RawEntry {
                name: "S1论坛签到".into(),
                author: Some("Antiky".into()),
                comments: None,
                version: Some("20230112".into()),
                date: Some("2023-01-11 20:30:00".into()),
                filename: "saraba1st.har".into(),
                url: Some("https://example.com/saraba1st.har".into()),
                comment_url: None,
                content: None,
            },
            None,
        );
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["name"], json!("S1论坛签到"));
        assert_eq!(value["author"], json!("Antiky"));
        assert_eq!(value["installed"], json!(false));
        assert_eq!(value["update_available"], json!(false));
        assert_eq!(value["installed_template_id"], Value::Null);
    }

    #[test]
    fn import_result_carries_the_ids_the_browser_acts_on() {
        // The WebUI opens the editor on whatever an import just produced, so the
        // template id has to survive serialisation; these field names are the
        // contract the browser reads (see `LibraryImportOutcome`).
        let result = LibraryImportResult {
            imported: 1,
            updated: 1,
            failed: Vec::new(),
            templates: vec![
                LibraryImportOutcome {
                    name: "S1论坛签到".into(),
                    template_id: 7,
                    updated: false,
                },
                LibraryImportOutcome {
                    name: "雨晨分享站".into(),
                    template_id: 9,
                    updated: true,
                },
            ],
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["imported"], json!(1));
        assert_eq!(value["updated"], json!(1));
        assert_eq!(value["templates"][0]["name"], json!("S1论坛签到"));
        assert_eq!(value["templates"][0]["template_id"], json!(7));
        assert_eq!(value["templates"][0]["updated"], json!(false));
        assert_eq!(value["templates"][1]["template_id"], json!(9));
        assert_eq!(value["templates"][1]["updated"], json!(true));
    }
}
