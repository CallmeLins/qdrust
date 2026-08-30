//! End-to-end reproduction of the 189天翼云 login crypto chain:
//! HAR steps 4-9 (RSA encrypt -> string/replace -> unicode) executed through
//! the real QdExecutor + UtilityPlugin, then the produced userkey/passkey are
//! decrypted with the private key to prove the chain matches QD semantics.

use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use qdrust_core::executor::{ExecutionContext, ExecutorOptions, QdExecutor};
use qdrust_core::qd_har::{QdHar, QdProgram};
use rsa::pkcs8::EncodePublicKey;
use serde_json::{Value, json};

/// Reproduce the exact template URLs/bodies/rules of the 189天翼云 HAR
/// (steps 4-9), the way webui qdTplToHar imports a QD-exported array.
fn build_chain_har(pub_key_body: &str) -> QdHar {
    let rsa_step = |name: &str, variable: &str| {
        json!({
            "checked": true,
            "comment": format!("[{variable}]RSA加密"),
            "request": {
                "method": "GET",
                "url": format!(
                    "api://util/rsa?f=encode&key=-----BEGIN%20PUBLIC%20KEY-----{{{{pubKey|urlencode}}}}-----END%20PUBLIC%20KEY-----&data={{{{{variable}|urlencode}}}}"
                ),
                "headers": [],
                "cookies": [],
            },
            "success_asserts": [{"re": "200", "from": "status"}],
            "extract_variables": [{"name": name, "re": "(.*)", "from": "content"}]
        })
    };
    let replace_step = |name: &str, source_expression: &str| {
        json!({
            "checked": true,
            "request": {
                "method": "POST",
                "url": "api://util/string/replace",
                "headers": [],
                "postData": {
                    "mimeType": "application/x-www-form-urlencoded",
                    "text": format!("r=json&p=&s={source_expression}&t=")
                }
            },
            "success_asserts": [{"re": "200", "from": "status"}, {"re": "\"状态\": \"OK\"", "from": "content"}],
            "extract_variables": [{"name": name, "re": "\"处理后字符串\": \"(.*)\"", "from": "content"}]
        })
    };
    let unicode_step = |name: &str, source_expression: &str| {
        json!({
            "checked": true,
            "request": {
                "method": "GET",
                "url": format!("api://util/unicode?content={source_expression}"),
                "headers": [],
                "cookies": [],
            },
            "success_asserts": [{"re": "200", "from": "status"}, {"re": "\"状态\": \"200\"", "from": "content"}],
            "extract_variables": [{"name": name, "re": "\"转换后\": \"(.*)\"", "from": "content"}]
        })
    };
    let hex_expression = |variable: &str| {
        format!(
            "{{{{unicode(b2a_hex(a2b_base64({variable}), sep=' ', bytes_per_sep=1))|urlencode}}}}"
        )
    };
    let entries = vec![
        rsa_step("userrsakey", "username"),
        replace_step("hexuserrsakey", &hex_expression("userrsakey")),
        unicode_step(
            "userkey",
            "{{pre|urlencode}}{{hexuserrsakey|replace(' ','')|urlencode}}",
        ),
        rsa_step("passrsakey", "password"),
        replace_step("hexpassrsakey", &hex_expression("passrsakey")),
        unicode_step(
            "passkey",
            "{{pre|urlencode}}{{hexpassrsakey|replace(' ','')|urlencode}}",
        ),
    ];
    let document = json!({"log": {"version": "1.2", "entries": entries}});
    // The imported document embeds the pubKey body through the template, so
    // inject it as a variable below instead of formatting it into the HAR.
    let _ = pub_key_body;
    QdHar::parse(document).expect("chain HAR must parse")
}

fn tight_hex(value: &str) -> Vec<u8> {
    let cleaned: String = value.split_whitespace().collect();
    (0..cleaned.len() / 2)
        .map(|index| u8::from_str_radix(&cleaned[index * 2..index * 2 + 2], 16).expect("hex byte"))
        .collect()
}

#[tokio::test]
async fn cloud189_api_chain_roundtrips_rsa_payload() -> Result<()> {
    let mut rng = rand::thread_rng();
    let private = rsa::RsaPrivateKey::new(&mut rng, 1024)?;
    let public = rsa::RsaPublicKey::from(&private);
    let public_pem = public.to_public_key_pem(rsa::pkcs8::LineEnding::LF)?;
    // encryptConf delivers the DER body only; the template re-adds the wrappers.
    let pub_key_body = public_pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>();
    let pre = "eJxTdHJpbmdQcmVmaXgxMjM0";

    let har = build_chain_har(&pub_key_body);
    let program = QdProgram::compile(&har)?;
    let executor = QdExecutor::with_options(ExecutorOptions {
        timeout: std::time::Duration::from_secs(10),
        allow_private_network: true,
        ..ExecutorOptions::default()
    })?;
    let variables = BTreeMap::from([
        ("username".to_string(), Value::from("13800138000")),
        ("password".to_string(), Value::from("p@ssW0rd!测试")),
        ("pubKey".to_string(), Value::from(pub_key_body.clone())),
        ("pre".to_string(), Value::from(pre)),
    ]);
    let mut context = ExecutionContext::new(variables);

    executor.execute(&program, &mut context).await?;

    let userkey = context
        .variables
        .get("userkey")
        .and_then(Value::as_str)
        .expect("userkey must be extracted");
    let passkey = context
        .variables
        .get("passkey")
        .and_then(Value::as_str)
        .expect("passkey must be extracted");

    ensure!(
        userkey.starts_with(pre) && passkey.starts_with(pre),
        "userkey/passkey must keep the pre prefix: {userkey}"
    );
    let user_cipher = tight_hex(&userkey[pre.len()..]);
    let pass_cipher = tight_hex(&passkey[pre.len()..]);
    ensure!(
        !user_cipher.is_empty() && !pass_cipher.is_empty(),
        "hex payload must be non-empty"
    );

    let decrypted_user = private.decrypt(rsa::pkcs1v15::Pkcs1v15Encrypt, &user_cipher)?;
    let decrypted_pass = private.decrypt(rsa::pkcs1v15::Pkcs1v15Encrypt, &pass_cipher)?;
    assert_eq!(decrypted_user, b"13800138000");
    assert_eq!(decrypted_pass, "p@ssW0rd!测试".as_bytes());
    Ok(())
}
