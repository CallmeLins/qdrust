//! Full-flow reproduction of the 189天翼云 login round (template steps 0-11)
//! against a local mock of the 189 endpoints. Proves that the imported QD
//! template drives the executor end to end: cookies flow through the 302s,
//! the api:// crypto chain fills userkey/passkey, and loginSubmit actually
//! posts a decryptable, non-empty userName/epd form body.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Result, ensure};
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use qdrust_core::executor::{ExecutionContext, ExecutorOptions, QdExecutor};
use qdrust_core::qd_har::{QdHar, QdProgram};
use rsa::pkcs8::EncodePublicKey;
use serde_json::{Value, json};
use tokio::sync::Mutex;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/189cloud.189.submit.har"
);
const PRE: &str = "eJxTdGVzdFByZWZpeDEyMzQ1Njc4";

#[derive(Default)]
struct Captured {
    login_body: Mutex<String>,
    login_cookie: Mutex<String>,
}

fn tight_hex(value: &str) -> Vec<u8> {
    let cleaned: String = value.split_whitespace().collect();
    (0..cleaned.len() / 2)
        .map(|index| u8::from_str_radix(&cleaned[index * 2..index * 2 + 2], 16).expect("hex byte"))
        .collect()
}

/// 302 response with a Location header (and optional cookie), like 189's
/// redirect chain.
fn redirect_response(location: String, cookie: Option<&str>) -> Response {
    let mut builder = axum::http::Response::builder()
        .status(StatusCode::FOUND)
        .header("location", location);
    if let Some(cookie) = cookie {
        builder = builder.header("set-cookie", cookie);
    }
    builder.body(axum::body::Body::empty()).unwrap()
}

#[tokio::test]
async fn cloud189_login_round_posts_decryptable_credentials() -> Result<()> {
    // RSA pair handed out by the mock encryptConf endpoint.
    let mut rng = rand::thread_rng();
    let private = rsa::RsaPrivateKey::new(&mut rng, 1024)?;
    let public = rsa::RsaPublicKey::from(&private);
    let public_pem = public.to_public_key_pem(rsa::pkcs8::LineEnding::LF)?;
    let pub_key_body = public_pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>();

    // Bind first so the redirect chain can point back at the mock server.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let base = format!("http://{address}");

    let captured = Arc::new(Captured::default());
    let login_state = captured.clone();
    let encrypt_key = pub_key_body.clone();
    let app = Router::new()
        .route(
            "/api/portal/loginUrl.action",
            get({
                let base = base.clone();
                move || {
                    let location = format!("{base}/web/login1.jsp?reqId=1&lt=2");
                    async move {
                        redirect_response(location, Some("JSESSIONID=mocksession; Path=/"))
                    }
                }
            }),
        )
        .route(
            "/web/login1.jsp",
            get({
                let base = base.clone();
                move || {
                    let location = format!("{base}/web/main.jsp");
                    async move { redirect_response(location, None) }
                }
            }),
        )
        .route(
            "/api/logbox/oauth2/appConf.do",
            post(|| async {
                Json(json!({
                    "paramId": "pid123",
                    "mailSuffix": "@189.cn",
                    "returnUrl": "https://m.cloud.189.cn/main.action",
                    "reqId": "req123"
                }))
            }),
        )
        .route(
            "/api/logbox/config/encryptConf.do",
            post(move || {
                let pub_key_body = encrypt_key.clone();
                async move { Json(json!({"pre": PRE, "pubKey": pub_key_body})) }
            }),
        )
        .route(
            "/api/logbox/oauth2/needcaptcha.do",
            post(|| async { Json(json!({"result": 0, "captchaToken": ""})) }),
        )
        .route(
            "/api/logbox/oauth2/loginSubmit.do",
            post(
                move |State(state): State<Arc<Captured>>,
                      headers: HeaderMap,
                      body: String| async move {
                    *state.login_body.lock().await = body;
                    *state.login_cookie.lock().await = headers
                        .get("cookie")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    Json(json!({
                        "result": 0,
                        "toUrl": "https://m.cloud.189.cn/main.action",
                        "msg": "登录成功"
                    }))
                },
            ),
        )
        .with_state(login_state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Load the real template, keep the login round (steps 0-11), and point
    // every 189 host at the mock server.
    let raw: Value = serde_json::from_slice(&std::fs::read(FIXTURE)?)?;
    let entries = raw
        .as_array()
        .expect("QD template exports a bare entry array")[0..=11]
        .to_vec();
    let entries = serde_json::to_string(&entries)?
        .replace("https://cloud.189.cn", &base)
        .replace("https://open.e.189.cn", &base);
    let document =
        json!({"log": {"version": "1.2", "entries": serde_json::from_str::<Value>(&entries)?}});
    let har = QdHar::parse_qd(document)?;
    let program = QdProgram::compile(&har)?;

    let executor = QdExecutor::with_options(ExecutorOptions {
        timeout: std::time::Duration::from_secs(10),
        allow_private_network: true,
        ..ExecutorOptions::default()
    })?;
    let variables = BTreeMap::from([
        ("username".to_string(), Value::from("13800138000")),
        ("password".to_string(), Value::from("p@ssW0rd!测试")),
        ("pubKey".to_string(), Value::from(pub_key_body)),
    ]);
    let mut context = ExecutionContext::new(variables);

    let results = executor.execute(&program, &mut context).await?;

    assert_eq!(results.len(), 12, "every login-round step must run");

    let body = captured.login_body.lock().await.clone();
    let cookie = captured.login_cookie.lock().await.clone();
    ensure!(
        cookie.contains("JSESSIONID=mocksession"),
        "cookies from the 302 chain must reach loginSubmit, got: {cookie}"
    );

    // Parse the posted form and verify userName/epd are the RSA payloads.
    let mut form = BTreeMap::new();
    for pair in body.split('&') {
        if let Some((name, value)) = pair.split_once('=') {
            form.insert(name.to_string(), value.to_string());
        }
    }
    let user_name = form.get("userName").cloned().unwrap_or_default();
    let epd = form.get("epd").cloned().unwrap_or_default();
    ensure!(
        user_name.starts_with(PRE) && epd.starts_with(PRE),
        "userName/epd must start with pre, body was: {body}"
    );
    let user_cipher = tight_hex(&user_name[PRE.len()..]);
    let pass_cipher = tight_hex(&epd[PRE.len()..]);
    let decrypted_user = private.decrypt(rsa::pkcs1v15::Pkcs1v15Encrypt, &user_cipher)?;
    let decrypted_pass = private.decrypt(rsa::pkcs1v15::Pkcs1v15Encrypt, &pass_cipher)?;
    assert_eq!(decrypted_user, b"13800138000");
    assert_eq!(decrypted_pass, "p@ssW0rd!测试".as_bytes());
    assert_eq!(form.get("paramId").map(String::as_str), Some("pid123"));
    assert_eq!(form.get("mailSuffix").map(String::as_str), Some("@189.cn"));
    Ok(())
}
