//! End-to-end run of the gmgard punch-in template reported in issue #39.
//!
//! The fixture is the template **verbatim** as the reporter pasted it (a bare
//! QD array, rules nested under `rule`, `request.data` bodies, no `mimeType` on
//! the `api://` steps, and an extract rule with an empty name). The only edit
//! this test makes is pointing the two `gmgard.moe` steps at a local stub,
//! because the point is the template's shape rather than gmgard's HTML.
//!
//! What it pins is the reason the template was stuck in the first place: the
//! notepad is how it hands a refreshed cookie to the next run. So it is run
//! twice against one store, and the second run has to send back what the first
//! run stored.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, ensure};
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use qdrust_core::executor::{ExecutionContext, ExecutorOptions, QdExecutor};
use qdrust_core::plugin::{NotepadSlot, NotepadStore};
use qdrust_core::qd_har::{QdHar, QdProgram};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gmgard.punch-in.har"
));

/// The cookie the notepad is seeded with, standing in for the value a first
/// manual run (or the author's own browser) would have left there.
const SEED: &str = ".AspNetCore.Identity.Application=SEED";

#[derive(Default)]
struct Stub {
    /// Every distinct ticket the login step hands back, so the two runs can be
    /// told apart in the final assertion.
    issued: AtomicUsize,
    /// The `cookie` header each `GET /` arrived with, in order.
    login_cookies: Mutex<Vec<String>>,
    /// The `cookie` header each punch arrived with, in order.
    punch_cookies: Mutex<Vec<String>>,
}

fn cookie_header(headers: &HeaderMap) -> String {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn respond(status: StatusCode, set_cookie: Option<String>, body: &str) -> Response {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8");
    if let Some(cookie) = set_cookie {
        builder = builder.header(header::SET_COOKIE, cookie);
    }
    builder
        .body(Body::from(body.to_string()))
        .expect("stub response")
}

/// The logged-in page: the two strings the template asserts on, plus the
/// refreshed identity cookie it extracts from the response headers.
async fn site_root(State(stub): State<Arc<Stub>>, headers: HeaderMap) -> Response {
    stub.login_cookies
        .lock()
        .unwrap()
        .push(cookie_header(&headers));
    let ticket = stub.issued.fetch_add(1, Ordering::SeqCst) + 1;
    respond(
        StatusCode::OK,
        Some(format!(
            ".AspNetCore.Identity.Application=TICK-{ticket}; expires=Wed, 01 Jan 2070 00:00:00 GMT; path=/; httponly"
        )),
        "<html><body>任务：1 收藏：2</body></html>",
    )
}

/// The punch endpoint. The body is shaped so all four of the template's
/// extracts match, and it deliberately lacks 「此日已签」 so the failed assert
/// stays quiet.
///
/// The shape is not arbitrary: every one of those patterns ends in a greedy
/// `(.*)` pinned by a two-character trailer (`,"e`, `,"p`, `,"s`, `,`), so each
/// trailer may appear exactly once in the line — a trailing `"end":0` would make
/// `tiveDays":(.*),"e` capture everything up to it. A real response keeps a
/// field after `success` for the same reason.
async fn punch(State(stub): State<Arc<Stub>>, headers: HeaderMap) -> Response {
    stub.punch_cookies
        .lock()
        .unwrap()
        .push(cookie_header(&headers));
    respond(
        StatusCode::OK,
        None,
        r#"{"status":"success","activeDays":7,"expBonus":1,"points":100,"success":true,"msg":"ok"}"#,
    )
}

async fn spawn_stub() -> (String, Arc<Stub>) {
    let stub = Arc::new(Stub::default());
    let app = Router::new()
        .route("/", get(site_root))
        .route("/api/PunchIn/Do", post(punch))
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let address = listener.local_addr().expect("stub address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("stub must serve");
    });
    (format!("http://{address}"), stub)
}

/// The fixture, with gmgard's origin swapped for the stub. The substitution goes
/// through the serialized form so every occurrence is caught — the URL and the
/// `origin`/`referer` header values — without touching `:authority`.
fn fixture_with_stub(base: &str) -> Value {
    let document: Value = serde_json::from_str(FIXTURE).expect("fixture must be JSON");
    let text = serde_json::to_string(&document)
        .expect("fixture must serialize")
        .replace("https://gmgard.moe", base);
    serde_json::from_str(&text).expect("stub-substituted fixture must be JSON")
}

#[derive(Default)]
struct MemoryNotepad {
    slots: Mutex<BTreeMap<i64, Option<String>>>,
}

impl std::fmt::Debug for MemoryNotepad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MemoryNotepad({:?})", self.slots.lock().unwrap())
    }
}

impl MemoryNotepad {
    fn stored(&self, slot: i64) -> Option<String> {
        self.slots.lock().unwrap().get(&slot).cloned().flatten()
    }
}

impl NotepadStore for MemoryNotepad {
    fn read<'a>(
        &'a self,
        slot: i64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<NotepadSlot>> + Send + 'a>> {
        Box::pin(async move {
            Ok(match self.slots.lock().unwrap().get(&slot) {
                None => NotepadSlot::Missing,
                Some(content) => NotepadSlot::Stored(content.clone()),
            })
        })
    }

    fn write<'a>(
        &'a self,
        slot: i64,
        content: Option<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            self.slots.lock().unwrap().insert(slot, content);
            Ok(())
        })
    }

    fn slots<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<i64>>> + Send + 'a>> {
        Box::pin(async move { Ok(self.slots.lock().unwrap().keys().copied().collect()) })
    }

    fn remove<'a>(
        &'a self,
        slot: i64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            self.slots.lock().unwrap().remove(&slot);
            Ok(())
        })
    }
}

/// One run of the template. `id_notepad` is the only input it declares: the
/// `qd_email`/`md5(qd_pwd)` arguments QD's editor puts in the notepad steps are
/// left undefined on purpose, because this port takes the account from the run.
async fn run_template(
    executor: &QdExecutor,
    program: &qdrust_core::qd_har::QdProgram,
) -> Result<BTreeMap<String, Value>> {
    let variables = BTreeMap::from([("id_notepad".to_string(), Value::from(1))]);
    let mut context = ExecutionContext::new(variables);
    executor.execute(program, &mut context).await?;
    Ok(context.variables)
}

#[tokio::test]
async fn the_reported_gmgard_template_carries_its_cookie_across_runs() -> Result<()> {
    let (base, stub) = spawn_stub().await;

    let har = QdHar::parse_qd(fixture_with_stub(&base))?;
    let program = QdProgram::compile(&har)?;

    let notepad = Arc::new(MemoryNotepad::default());
    notepad
        .slots
        .lock()
        .unwrap()
        .insert(1, Some(SEED.to_string()));

    let executor = QdExecutor::with_options(ExecutorOptions {
        timeout: Duration::from_secs(10),
        allow_private_network: true,
        notepad: Some(notepad.clone() as Arc<dyn NotepadStore>),
        ..ExecutorOptions::default()
    })?;

    // --- First run: spend the seeded cookie, store the refreshed one. ---
    let first = run_template(&executor, &program).await?;
    let log = first
        .get("__log__")
        .and_then(Value::as_str)
        .expect("the unicode step must leave a log line");
    assert_eq!(
        log, "绅士之庭已签到7天，获得1积分，总共积分100 。",
        "the punch result must reach the log"
    );
    assert_eq!(stub.login_cookies.lock().unwrap()[0], SEED);
    assert_eq!(
        notepad.stored(1).as_deref(),
        Some(".AspNetCore.Identity.Application=TICK-1"),
        "the write step must overwrite the slot with the refreshed cookie"
    );
    assert_eq!(
        stub.punch_cookies.lock().unwrap()[0],
        ".AspNetCore.Identity.Application=TICK-1",
        "the punch must use the cookie the same run just stored"
    );

    // --- Second run: the whole point — it starts from what run one left. ---
    let second = run_template(&executor, &program).await?;
    assert!(
        second.contains_key("__log__"),
        "second run must produce its own log line"
    );
    assert_eq!(
        stub.login_cookies.lock().unwrap()[1],
        ".AspNetCore.Identity.Application=TICK-1",
        "the second run must log in with the cookie the first run stored"
    );
    assert_eq!(
        notepad.stored(1).as_deref(),
        Some(".AspNetCore.Identity.Application=TICK-2"),
        "and store the cookie it refreshed in turn"
    );
    Ok(())
}

/// The `{% else %}` half: when the login response carries no refreshed cookie,
/// `cookies2` is never extracted, the `{% if cookies2 %}` branch is skipped, and
/// the punch goes out with the stored cookie instead.
#[tokio::test]
async fn the_else_branch_punches_with_the_stored_cookie() -> Result<()> {
    let stub = Arc::new(Stub::default());
    // A login page that asserts fine but hands back nothing to extract.
    let app = Router::new()
        .route(
            "/",
            get(|headers: HeaderMap| async move {
                let _ = cookie_header(&headers);
                respond(
                    StatusCode::OK,
                    None,
                    "<html><body>任务：1 收藏：2</body></html>",
                )
            }),
        )
        .route("/api/PunchIn/Do", post(punch))
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("stub must serve");
    });

    let har = QdHar::parse_qd(fixture_with_stub(&base))?;
    let program = QdProgram::compile(&har)?;
    let notepad = Arc::new(MemoryNotepad::default());
    notepad
        .slots
        .lock()
        .unwrap()
        .insert(1, Some(SEED.to_string()));
    let executor = QdExecutor::with_options(ExecutorOptions {
        timeout: Duration::from_secs(10),
        allow_private_network: true,
        notepad: Some(notepad.clone() as Arc<dyn NotepadStore>),
        ..ExecutorOptions::default()
    })?;

    run_template(&executor, &program).await?;

    let punches = stub.punch_cookies.lock().unwrap().clone();
    ensure!(
        punches.len() == 1,
        "only the else branch may punch, got {punches:?}"
    );
    assert_eq!(
        punches[0], SEED,
        "the else branch punches with the stored cookie"
    );
    assert_eq!(
        notepad.stored(1).as_deref(),
        Some(SEED),
        "with nothing extracted the write step stores the replaced string, which is unchanged"
    );
    // `json!` is used so the crate's own JSON dependency stays exercised here.
    assert_eq!(json!({"id_notepad": 1})["id_notepad"], Value::from(1));
    Ok(())
}
