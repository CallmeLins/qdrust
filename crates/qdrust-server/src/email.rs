use anyhow::{Context, Result, ensure};
use lettre::{
    Message, SmtpTransport, Transport,
    address::Address,
    message::Mailbox,
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    },
};
use std::env;

/// Email sender configuration resolved from environment variables.
#[derive(Clone, Debug, Default)]
pub struct EmailConfig {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: Option<String>,
    pub starttls: bool,
}

impl EmailConfig {
    pub fn from_env() -> Self {
        Self {
            host: env::var("QDRUST_SMTP_HOST").ok().filter(|s| !s.is_empty()),
            port: env::var("QDRUST_SMTP_PORT")
                .ok()
                .and_then(|v| v.parse().ok()),
            username: env::var("QDRUST_SMTP_USERNAME")
                .ok()
                .filter(|s| !s.is_empty()),
            password: env::var("QDRUST_SMTP_PASSWORD")
                .ok()
                .filter(|s| !s.is_empty()),
            from: env::var("QDRUST_SMTP_FROM").ok().filter(|s| !s.is_empty()),
            starttls: env::var("QDRUST_SMTP_STARTTLS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(true),
        }
    }

    pub fn enabled(&self) -> bool {
        self.host.is_some()
    }
}

pub struct EmailClient {
    config: EmailConfig,
}

impl EmailClient {
    pub fn new(config: EmailConfig) -> Result<Self> {
        ensure!(
            !config.enabled() || config.from.is_some(),
            "email requires QDRUST_SMTP_FROM when SMTP is enabled"
        );
        Ok(Self { config })
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled()
    }

    pub fn send(&self, to: &str, from: Option<&str>, subject: &str, body: &str) -> Result<()> {
        if !self.enabled() {
            return Ok(());
        }
        let from = from.unwrap_or_else(|| self.config.from.as_deref().unwrap());
        let sender: Mailbox = from.parse().context("invalid from email address")?;
        let recipient: Mailbox = to.parse().context("invalid to email address")?;
        let message = Message::builder()
            .from(sender)
            .to(recipient)
            .subject(subject)
            .header(lettre::message::header::ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .context("cannot build email message")?;

        let host = self
            .config
            .host
            .as_deref()
            .context("SMTP host is not configured")?;
        let tls = Tls::Opportunistic(
            TlsParameters::new(host.to_string()).context("invalid SMTP TLS parameters")?,
        );
        let mut builder = SmtpTransport::builder_dangerous(host)
            .port(self.config.port.unwrap_or(587))
            .tls(tls);
        if let (Some(username), Some(password)) = (&self.config.username, &self.config.password) {
            builder = builder.credentials(Credentials::new(username.clone(), password.clone()));
        }
        let mailer = builder.build();
        mailer.send(&message).context("email delivery failed")?;
        Ok(())
    }
}

/// Parse a raw address (mailbox or bare email) into a valid SMTP `to` address.
pub fn normalize_recipient(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    // Accept "Name <a@b.c>" or bare "a@b.c".
    if value.parse::<Address>().is_ok() {
        return Some(value.to_string());
    }
    if let Some((_, addr)) = value.rsplit_once('<') {
        let addr = addr.trim_end_matches('>').trim();
        if addr.parse::<Address>().is_ok() {
            return Some(addr.to_string());
        }
    }
    None
}
