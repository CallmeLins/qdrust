use axum::body::Body;
use axum::http::{Request, Response};
use http_body_util::BodyExt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use tower::{Layer, Service};

use crate::api::RuntimeSettings;

/// Injects the Google Analytics snippet into served `text/html` responses
/// (WebUI index.html) when a GA key is configured. The key is resolved lazily
/// so it can be changed via admin settings at runtime.
#[derive(Clone)]
pub struct InjectGaLayer {
    settings: Arc<std::sync::RwLock<RuntimeSettings>>,
}

impl InjectGaLayer {
    pub fn new(settings: Arc<std::sync::RwLock<RuntimeSettings>>) -> Self {
        Self { settings }
    }
}

impl<S> Layer<S> for InjectGaLayer {
    type Service = InjectGaService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        InjectGaService {
            inner,
            settings: self.settings.clone(),
        }
    }
}

#[derive(Clone)]
pub struct InjectGaService<S> {
    inner: S,
    settings: Arc<std::sync::RwLock<RuntimeSettings>>,
}

impl<S, ReqBody> Service<Request<ReqBody>> for InjectGaService<S>
where
    S: Service<Request<ReqBody>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let key = self
            .settings
            .read()
            .map(|s| s.ga_key.clone())
            .unwrap_or(None);
        let mut inner = self.inner.clone();
        Box::pin(async move {
            let response = inner.call(req).await?;
            let Some(key) = key else {
                return Ok(response);
            };
            let is_html = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.contains("text/html"))
                .unwrap_or(false);
            if !is_html {
                return Ok(response);
            }
            let (parts, body) = response.into_parts();
            let bytes = match body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(_) => return Ok(Response::from_parts(parts, Body::empty())),
            };
            let html = String::from_utf8_lossy(&bytes);
            let script = format!(
                r#"<script async src="https://www.googletagmanager.com/gtag/js?id={key}"></script>
<script>
  window.dataLayer = window.dataLayer || [];
  function gtag() {{ dataLayer.push(arguments); }}
  gtag('js', new Date());
  gtag('config', '{key}');
</script>
</head>"#
            );
            let injected = if html.contains("</head>") {
                html.replacen("</head>", &script, 1)
            } else {
                format!("{html}\n{script}")
            };
            let content_length = injected.len();
            let mut response = Response::from_parts(parts, Body::from(injected.into_bytes()));
            response.headers_mut().insert(
                "content-length",
                axum::http::HeaderValue::from_str(&content_length.to_string())
                    .unwrap_or_else(|_| axum::http::HeaderValue::from_static("0")),
            );
            Ok(response)
        })
    }
}
