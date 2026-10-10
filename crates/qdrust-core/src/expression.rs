use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{Local, TimeZone};
use fake::Fake;
use minijinja::value::{Enumerator, Kwargs, Object, ObjectRepr, Rest, ValueKind};
use minijinja::{Environment, Error, ErrorKind, State, Value as JinjaValue};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use rand::Rng;
use regex::Regex;
use serde_json::Value;
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use uuid::Uuid;

/// Registers a QD-compatible global function AND filter.
///
/// QD builds its jinja environment with `self.jinja_env.globals = utils.jinja_globals`
/// followed by `self.jinja_env.filters.update(utils.jinja_globals)` (see
/// qd/libs/fetcher.py), so every QD global is also usable as a filter, e.g.
/// `{{ password|md5 }}` or `{{ raw|b64encode }}`.
macro_rules! qd_fn {
    ($env:ident, $name:literal, $func:expr) => {{
        $env.add_function($name, $func);
        $env.add_filter($name, $func);
    }};
}

/// The optional arguments of a QD helper, read the way Python would.
///
/// QD's helpers are Python functions, so a template may pass an optional
/// argument **either** by position or by name — `urlencode(x, for_qs=True)` and
/// `urlencode(x, 'utf-8', True)` both appear in the wild. MiniJinja collects the
/// named ones into a single trailing kwargs value, which carries no information
/// about which parameter they belong to, so the reading is positional with the
/// kwargs map as the fallback — and a name the helper does not take is refused
/// rather than silently ignored.
struct QdCall<'a> {
    positional: std::slice::Iter<'a, JinjaValue>,
    kwargs: Option<&'a JinjaValue>,
}

impl<'a> QdCall<'a> {
    fn new(rest: &'a [JinjaValue]) -> Self {
        let split = rest.iter().position(JinjaValue::is_kwargs);
        let (positional, kwargs) = match split {
            Some(index) => (&rest[..index], rest.get(index)),
            None => (rest, None),
        };
        Self {
            positional: positional.iter(),
            kwargs,
        }
    }

    /// The next optional argument, by position or by name.
    ///
    /// An undefined value is reported as "not passed", which is what Python
    /// does for a keyword whose value happens to be undefined.
    fn next(&mut self, name: &str) -> Option<JinjaValue> {
        if let Some(value) = self.positional.next() {
            return Some(value.clone());
        }
        let value = self.kwargs?.get_attr(name).ok()?;
        (!value.is_undefined()).then_some(value)
    }

    /// More positional arguments than the helper takes is the mistake that used
    /// to reach the user as a bare "too many arguments" from inside MiniJinja,
    /// with no mention of which call made it.
    fn finish(self, accepted: &[&str]) -> Result<(), Error> {
        if !self.positional.as_slice().is_empty() {
            return Err(Error::new(
                ErrorKind::TooManyArguments,
                format!(
                    "expected at most {} argument(s), got {} more",
                    accepted.len(),
                    self.positional.len()
                ),
            ));
        }
        let Some(kwargs) = self.kwargs else {
            return Ok(());
        };
        for key in kwargs.try_iter()? {
            let key = key.to_string();
            if !accepted.contains(&key.as_str()) {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    format!("unexpected keyword argument {key:?} (accepted: {accepted:?})"),
                ));
            }
        }
        Ok(())
    }
}

/// `urllib.parse.quote` as QD calls it: the unreserved set stays literal, and
/// "/" is the only extra character that stays literal — unless `for_qs` asks for
/// it to be quoted as `%2F`.
fn url_quote(value: &str, for_qs: bool) -> String {
    const SAFE: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~')
        .remove(b'/');
    const SAFE_FOR_QS: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    if for_qs {
        utf8_percent_encode(value, SAFE_FOR_QS).to_string()
    } else {
        utf8_percent_encode(value, SAFE).to_string()
    }
}

/// QD's `urlencode`: quote a string, or build a query string.
///
/// Given a string (or anything non-iterable) it quotes that string. Given a
/// mapping, or an iterable of `(key, value)` pairs, it joins them as
/// `k=v&k=v` — quoting both sides, and with "/" quoted, exactly as QD's
/// `urlencode_with_encoding` does.
fn urlencode_value(value: &JinjaValue, for_qs: bool) -> Result<String, Error> {
    let pairs: Vec<(JinjaValue, JinjaValue)> = match value.kind() {
        ValueKind::Map => value
            .try_iter()?
            .map(|key| Ok((key.clone(), value.get_item(&key)?)))
            .collect::<std::result::Result<_, Error>>()?,
        ValueKind::Seq => value
            .try_iter()?
            .map(|pair| {
                let mut parts = pair.try_iter()?;
                match (parts.next(), parts.next()) {
                    (Some(key), Some(item)) if parts.next().is_none() => Ok((key, item)),
                    _ => Err(Error::new(
                        ErrorKind::InvalidOperation,
                        "urlencode expects an iterable of (key, value) pairs",
                    )),
                }
            })
            .collect::<std::result::Result<_, Error>>()?,
        _ => return Ok(url_quote(&value.to_string(), for_qs)),
    };
    Ok(pairs
        .iter()
        .map(|(key, item)| {
            format!(
                "{}={}",
                url_quote(&key.to_string(), true),
                url_quote(&item.to_string(), true)
            )
        })
        .collect::<Vec<_>>()
        .join("&"))
}

/// QD's `urlencode` takes the charset as its second argument.
///
/// Only UTF-8 is implemented. Refusing the rest is deliberate: quoting a GBK
/// string as UTF-8 produces a URL that decodes to mojibake, which is worse than
/// an error the template author can see.
fn check_urlencode_encoding(value: &JinjaValue) -> Result<(), Error> {
    if value.is_none() || value.is_undefined() {
        return Ok(());
    }
    let encoding = value.to_string();
    let normalized = encoding.trim().to_ascii_lowercase();
    if normalized.is_empty() || normalized == "utf-8" || normalized == "utf8" {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::InvalidOperation,
        format!("urlencode only supports utf-8, got {encoding:?}"),
    ))
}

/// Jinja2's `default`: replace an undefined value, or — with `boolean=True` —
/// any falsey one.
///
/// QD inherits this from Jinja2 rather than from its own `jinja_globals`, so the
/// third parameter exists there and templates use it. `None` counts as undefined
/// here, which is what this engine has always done; Jinja2 would keep it.
fn default_filter(value: JinjaValue, rest: &[JinjaValue]) -> Result<JinjaValue, Error> {
    let mut call = QdCall::new(rest);
    let default_value = call
        .next("default_value")
        .unwrap_or_else(|| JinjaValue::from(""));
    let boolean = call
        .next("boolean")
        .map(|value| value.is_true())
        .unwrap_or(false);
    call.finish(&["default_value", "boolean"])?;
    if value.is_undefined() || value.is_none() || (boolean && !value.is_true()) {
        Ok(default_value)
    } else {
        Ok(value)
    }
}

/// MiniJinja's error, with the source line it points at folded in.
///
/// The debug information is only produced by the **alternate** formatter, and
/// anyhow prints its sources with the plain one — so unless the text is captured
/// here it never reaches the run log, which is the only place a failed run is
/// ever read from.
fn minijinja_error(err: Error) -> anyhow::Error {
    anyhow::anyhow!("{err:#}")
}

pub struct QdExpressionEngine {
    environment: Environment<'static>,
}

impl Default for QdExpressionEngine {
    fn default() -> Self {
        let mut environment = Environment::new();
        // Keep the source of every template MiniJinja compiles, so a render
        // error can point at the line that failed. Without it the run log only
        // says "too many arguments" and the reader has to find the call by hand;
        // with it the offending line travels with the error. `QdExpressionError`
        // is what makes sure the text survives the trip.
        environment.set_debug(true);

        // Type conversion functions
        qd_fn!(environment, "int", |value: JinjaValue| parse_i64(&value));
        qd_fn!(environment, "float", |value: JinjaValue| parse_f64(&value));
        // QD to_bool: only 'yes'/'on'/'1'/'true' (case-insensitive) are true.
        qd_fn!(environment, "bool", |value: JinjaValue| {
            Ok::<_, Error>(qd_bool(&value))
        });
        environment.add_function("list", |value: JinjaValue| {
            Ok::<_, Error>(JinjaValue::from_iter(value.try_iter()?))
        });
        environment.add_function("len", |value: JinjaValue| {
            value
                .len()
                .ok_or_else(|| Error::new(ErrorKind::InvalidOperation, "value has no length"))
        });

        // Encoding functions - base64 (QD b64encode/b64decode tolerate whitespace)
        qd_fn!(environment, "b64encode", |value: JinjaValue| {
            let s = value.to_string();
            Ok::<_, Error>(BASE64.encode(s.as_bytes()))
        });
        qd_fn!(environment, "b64decode", |value: JinjaValue| {
            let s: String = value
                .to_string()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            BASE64
                .decode(s.as_bytes())
                .map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        format!("base64 decode failed: {e}"),
                    )
                })
                .and_then(|bytes| {
                    String::from_utf8(bytes).map_err(|e| {
                        Error::new(ErrorKind::InvalidOperation, format!("invalid UTF-8: {e}"))
                    })
                })
        });

        // Encoding functions - binascii (QD re-exports binascii.b2a_hex and friends)
        // binascii.b2a_hex(data, sep='', bytes_per_sep=0); sep is inserted every
        // `bytes_per_sep` input bytes, counting from the right for positive values.
        qd_fn!(
            environment,
            "b2a_hex",
            |value: JinjaValue, kwargs: Kwargs| {
                let data = value_bytes(&value);
                let sep = kwargs.get::<Option<String>>("sep")?.unwrap_or_default();
                let bytes_per_sep = kwargs
                    .get::<Option<i64>>("bytes_per_sep")?
                    .unwrap_or_default();
                Ok::<_, Error>(hex_with_sep(&data, &sep, bytes_per_sep))
            }
        );
        qd_fn!(environment, "a2b_hex", |value: JinjaValue| {
            let s = value.to_string();
            hex::decode(s.trim())
                .map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        format!("hex decode failed: {e}"),
                    )
                })
                .map(JinjaValue::from_bytes)
        });
        // binascii.a2b_base64: base64 decode to raw bytes (bytes survive for
        // chained calls such as b2a_hex(a2b_base64(x))).
        qd_fn!(environment, "a2b_base64", |value: JinjaValue| {
            let s: String = value
                .to_string()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            BASE64
                .decode(s.as_bytes())
                .map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        format!("base64 decode failed: {e}"),
                    )
                })
                .map(JinjaValue::from_bytes)
        });
        // binascii.b2a_base64: base64 encode, newline terminated (Python appends \n).
        qd_fn!(environment, "b2a_base64", |value: JinjaValue| {
            Ok::<_, Error>(format!("{}\n", BASE64.encode(value_bytes(&value))))
        });
        // binascii.b2a_uu / a2b_uu: single-line uuencode/uudecode.
        qd_fn!(environment, "b2a_uu", |value: JinjaValue| {
            Ok::<_, Error>(uuencode_line(&value_bytes(&value)))
        });
        qd_fn!(environment, "a2b_uu", |value: JinjaValue| {
            uudecode_line(&value.to_string()).map(JinjaValue::from_bytes)
        });
        // binascii.b2a_qp / a2b_qp: quoted-printable codec.
        qd_fn!(
            environment,
            "b2a_qp",
            |value: JinjaValue, kwargs: Kwargs| {
                let quotetabs = kwargs.get::<Option<bool>>("quotetabs")?.unwrap_or_default();
                let istext = kwargs.get::<Option<bool>>("istext")?.unwrap_or_default();
                let data = value_bytes(&value);
                Ok::<_, Error>(qp_encode(&data, quotetabs, istext))
            }
        );
        qd_fn!(environment, "a2b_qp", |value: JinjaValue| {
            qp_decode(&value.to_string()).map(JinjaValue::from_bytes)
        });
        // binascii.crc32 / crc_hqx
        qd_fn!(environment, "crc32", |value: JinjaValue| {
            Ok::<_, Error>(crc32(&value_bytes(&value)) as i64)
        });
        qd_fn!(
            environment,
            "crc_hqx",
            |value: JinjaValue, initial: Option<i64>| {
                Ok::<_, Error>(
                    crc_hqx(&value_bytes(&value), initial.unwrap_or_default() as u16) as i64,
                )
            }
        );
        // Python builtin format(value, format_spec) - common subset.
        qd_fn!(
            environment,
            "format",
            |value: JinjaValue, spec: Option<String>| {
                python_format(&value, spec.as_deref().unwrap_or(""))
            }
        );

        // URL encoding (QD urlencode = urllib.parse.quote with safe="/", so "/"
        // stays literal and space becomes %20, not +). QD's helper also takes
        // the charset and `for_qs` — the latter quotes "/" as %2F — and builds a
        // query string when handed a mapping instead of a string.
        qd_fn!(
            environment,
            "urlencode",
            |value: JinjaValue, rest: Rest<JinjaValue>| {
                let mut call = QdCall::new(&rest);
                let encoding = call.next("encoding");
                let for_qs = call
                    .next("for_qs")
                    .map(|value| value.is_true())
                    .unwrap_or(false);
                call.finish(&["encoding", "for_qs"])?;
                if let Some(encoding) = encoding.as_ref() {
                    check_urlencode_encoding(encoding)?;
                }
                urlencode_value(&value, for_qs)
            }
        );
        environment.add_function("url_decode", |value: JinjaValue| {
            let s = value.to_string();
            percent_encoding::percent_decode_str(&s)
                .decode_utf8()
                .map(|decoded| decoded.into_owned())
                .map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        format!("url_decode failed: {e}"),
                    )
                })
        });
        environment.add_function("url_encode", |value: JinjaValue| {
            let s = value.to_string();
            const FRAGMENT: &AsciiSet = &NON_ALPHANUMERIC
                .remove(b'-')
                .remove(b'_')
                .remove(b'.')
                .remove(b'~');
            Ok::<_, Error>(utf8_percent_encode(&s, FRAGMENT).to_string())
        });

        // Quote Chinese characters for URL
        environment.add_function("quote_chinese", |value: JinjaValue| {
            let s = value.to_string();
            let encoded = s
                .chars()
                .map(|c| {
                    if c.is_ascii() {
                        c.to_string()
                    } else {
                        c.to_string()
                            .bytes()
                            .map(|b| format!("%{:02X}", b))
                            .collect::<String>()
                    }
                })
                .collect::<String>();
            Ok::<_, Error>(encoded)
        });

        // UTF-8 encoding (identity in Rust since strings are UTF-8)
        environment.add_function("utf8", |value: JinjaValue| {
            Ok::<_, Error>(value.to_string())
        });

        // QD conver2unicode: decode \uXXXX / \xNN escape sequences embedded in the
        // text; plain ASCII and real characters pass through unchanged.
        qd_fn!(environment, "unicode", |value: JinjaValue| {
            Ok::<_, Error>(conver2unicode(&value.to_string()))
        });

        // Hash functions
        qd_fn!(environment, "md5", |value: JinjaValue| {
            let s = value.to_string();
            let digest = md5::compute(s.as_bytes());
            Ok::<_, Error>(format!("{:x}", digest))
        });
        qd_fn!(environment, "sha1", |value: JinjaValue| {
            use sha1::Digest;
            let s = value.to_string();
            let digest = Sha1::digest(s.as_bytes());
            Ok::<_, Error>(hex::encode(digest))
        });
        qd_fn!(
            environment,
            "hash",
            |value: JinjaValue, hashtype: Option<String>| {
                use sha1::Digest as Sha1Digest;

                let s = value.to_string();
                let hashtype = hashtype.unwrap_or_else(|| "sha1".to_string());
                match hashtype.as_str() {
                    "md5" => {
                        let digest = md5::compute(s.as_bytes());
                        Ok::<_, Error>(format!("{:x}", digest))
                    }
                    "sha1" => {
                        let digest = Sha1::digest(s.as_bytes());
                        Ok::<_, Error>(hex::encode(digest))
                    }
                    "sha256" => {
                        let digest = Sha256::digest(s.as_bytes());
                        Ok::<_, Error>(hex::encode(digest))
                    }
                    "sha512" => {
                        let digest = Sha512::digest(s.as_bytes());
                        Ok::<_, Error>(hex::encode(digest))
                    }
                    _ => Err(Error::new(
                        ErrorKind::InvalidOperation,
                        format!("unsupported hash type: {hashtype}"),
                    )),
                }
            }
        );
        // TOTP (RFC 6238), computed locally so a 2FA secret never has to be
        // sent to an external API. `{{ totp(secret) }}` is the ergonomic form
        // for a header or body; `api://util/totp` is the step form. The last
        // argument is the unix time, which makes the result reproducible.
        qd_fn!(
            environment,
            "totp",
            |secret: String,
             digits: Option<i64>,
             period: Option<i64>,
             algo: Option<String>,
             at: Option<i64>| {
                let at = at.map(|value| value.max(0) as u64).unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|elapsed| elapsed.as_secs())
                        .unwrap_or(0)
                });
                crate::totp::code(
                    &secret,
                    digits.unwrap_or(6).clamp(6, 10) as u32,
                    period.unwrap_or(30).max(1) as u64,
                    algo.as_deref().unwrap_or("sha1"),
                    at,
                )
                .map_err(|err| Error::new(ErrorKind::InvalidOperation, err.to_string()))
            }
        );
        // QD get_encrypted_password relies on passlib modular-crypt formats which
        // qdrust does not implement; fail with a clear message instead of an
        // "unknown function" render error.
        qd_fn!(
            environment,
            "password_hash",
            |_value: JinjaValue, hashtype: Option<String>, _kwargs: Kwargs| {
                Err::<JinjaValue, Error>(Error::new(
                    ErrorKind::InvalidOperation,
                    format!(
                        "QD function password_hash (passlib crypt, type {}) is not supported by qdrust",
                        hashtype.unwrap_or_else(|| "sha512".into())
                    ),
                ))
            }
        );

        // QD AES helpers (utils._aes_encrypt/_aes_decrypt): CBC/ECB with pkcs7
        // padding, base64 (encodebytes style, 76-char lines) or hex output.
        qd_fn!(
            environment,
            "aes_encrypt",
            |word: JinjaValue, key: String, kwargs: Kwargs| {
                let mode = kwargs
                    .get::<Option<String>>("mode")?
                    .unwrap_or_else(|| "CBC".into());
                let iv = kwargs.get::<Option<String>>("iv")?;
                let output_format = kwargs
                    .get::<Option<String>>("output_format")?
                    .unwrap_or_else(|| "base64".into());
                let padding = kwargs.get::<Option<bool>>("padding")?.unwrap_or(true);
                let padding_style = kwargs
                    .get::<Option<String>>("padding_style")?
                    .unwrap_or_else(|| "pkcs7".into());
                let plain = value_bytes(&word);
                let cipher = aes_apply(
                    &key,
                    &mode,
                    iv.as_deref(),
                    &plain,
                    padding,
                    &padding_style,
                    true,
                )?;
                Ok::<_, Error>(aes_format_output(&cipher, &output_format))
            }
        );
        qd_fn!(
            environment,
            "aes_decrypt",
            |word: JinjaValue, key: String, kwargs: Kwargs| {
                let mode = kwargs
                    .get::<Option<String>>("mode")?
                    .unwrap_or_else(|| "CBC".into());
                let iv = kwargs.get::<Option<String>>("iv")?;
                let input_format = kwargs
                    .get::<Option<String>>("input")?
                    .or_else(|| kwargs.get::<Option<String>>("input_format").ok().flatten())
                    .unwrap_or_else(|| "base64".into());
                let padding = kwargs.get::<Option<bool>>("padding")?.unwrap_or(true);
                let padding_style = kwargs
                    .get::<Option<String>>("padding_style")?
                    .unwrap_or_else(|| "pkcs7".into());
                let cleaned: String = word
                    .to_string()
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                let cipher = match input_format.as_str() {
                    "base64" => BASE64.decode(cleaned.as_bytes()).map_err(|e| {
                        Error::new(
                            ErrorKind::InvalidOperation,
                            format!("base64 decode failed: {e}"),
                        )
                    })?,
                    "hex" => hex::decode(cleaned.trim()).map_err(|e| {
                        Error::new(
                            ErrorKind::InvalidOperation,
                            format!("hex decode failed: {e}"),
                        )
                    })?,
                    other => {
                        return Err(Error::new(
                            ErrorKind::InvalidOperation,
                            format!("unsupported aes input format: {other}"),
                        ));
                    }
                };
                let plain = aes_apply(
                    &key,
                    &mode,
                    iv.as_deref(),
                    &cipher,
                    padding,
                    &padding_style,
                    false,
                )?;
                Ok::<_, Error>(String::from_utf8_lossy(&plain).to_string())
            }
        );

        // Time functions
        qd_fn!(environment, "timestamp", |type_str: Option<String>| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap();
            match type_str.as_deref() {
                Some("float") => Ok::<_, Error>(JinjaValue::from(now.as_secs_f64())),
                _ => Ok::<_, Error>(JinjaValue::from(now.as_secs())),
            }
        });

        qd_fn!(environment, "date_time", |date: Option<JinjaValue>,
                                          time: Option<JinjaValue>,
                                          time_difference: Option<
            JinjaValue,
        >| {
            let show_date = date.as_ref().map(|v| v.is_true()).unwrap_or(true);
            let show_time = time.as_ref().map(|v| v.is_true()).unwrap_or(true);
            let time_diff = time_difference
                .and_then(|v| v.as_str().and_then(|s| s.parse::<i64>().ok()))
                .unwrap_or(0);

            let now = Local::now() + chrono::Duration::hours(time_diff);

            if show_date {
                if show_time {
                    Ok::<_, Error>(now.format("%Y-%m-%d %H:%M:%S").to_string())
                } else {
                    Ok::<_, Error>(now.format("%Y-%m-%d").to_string())
                }
            } else if show_time {
                Ok::<_, Error>(now.format("%H:%M:%S").to_string())
            } else {
                Ok::<_, Error>(String::new())
            }
        });

        environment.add_function("strftime", |format: String, second: Option<JinjaValue>| {
            let timestamp = if let Some(sec) = second {
                sec.to_string()
                    .parse::<i64>()
                    .map_err(|_| Error::new(ErrorKind::InvalidOperation, "invalid epoch value"))?
            } else {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64
            };

            let datetime = Local
                .timestamp_opt(timestamp, 0)
                .single()
                .ok_or_else(|| Error::new(ErrorKind::InvalidOperation, "invalid timestamp"))?;

            Ok::<_, Error>(datetime.format(&format).to_string())
        });

        // Math operations (QD semantics: variadic, skip-chain on non-numbers,
        // results formatted like Python's f"{value:f}" - always 6 decimals).
        qd_fn!(environment, "add", |values: Rest<JinjaValue>| {
            Ok::<_, Error>(qd_arith(&values, QdArith::Add))
        });

        qd_fn!(environment, "sub", |values: Rest<JinjaValue>| {
            Ok::<_, Error>(qd_arith(&values, QdArith::Sub))
        });

        qd_fn!(environment, "multiply", |values: Rest<JinjaValue>| {
            Ok::<_, Error>(qd_arith(&values, QdArith::Mul))
        });

        qd_fn!(environment, "divide", |values: Rest<JinjaValue>| {
            Ok::<_, Error>(qd_arith(&values, QdArith::Div))
        });

        qd_fn!(environment, "is_num", |value: JinjaValue| {
            Ok::<_, Error>(qd_is_num(&value))
        });

        // Regex functions (QD order: value first, pattern second).
        qd_fn!(
            environment,
            "regex_replace",
            |value: JinjaValue, pattern: String, replacement: String, kwargs: Kwargs| {
                let count = kwargs.get::<Option<i64>>("count")?.unwrap_or_default();
                let ignorecase = kwargs
                    .get::<Option<bool>>("ignorecase")?
                    .unwrap_or_default();
                let multiline = kwargs.get::<Option<bool>>("multiline")?.unwrap_or_default();
                let re = qd_regex(&pattern, ignorecase, multiline)?;
                let subject = value.to_string();
                let repl = python_replacement(&replacement);
                let replaced = if count > 0 {
                    re.replacen(&subject, count as usize, repl.as_str())
                        .to_string()
                } else {
                    re.replace_all(&subject, repl.as_str()).to_string()
                };
                Ok::<_, Error>(replaced)
            }
        );

        qd_fn!(
            environment,
            "regex_search",
            |value: JinjaValue, pattern: String, backrefs: Rest<String>, kwargs: Kwargs| {
                let ignorecase = kwargs
                    .get::<Option<bool>>("ignorecase")?
                    .unwrap_or_default();
                let multiline = kwargs.get::<Option<bool>>("multiline")?.unwrap_or_default();
                let re = qd_regex(&pattern, ignorecase, multiline)?;
                let subject = value.to_string();
                let Some(caps) = re.captures(&subject) else {
                    // QD returns None implicitly when nothing matches.
                    return Ok::<_, Error>(JinjaValue::from(()));
                };
                if backrefs.is_empty() {
                    return Ok::<_, Error>(JinjaValue::from(
                        caps.get(0).map(|m| m.as_str()).unwrap_or(""),
                    ));
                }
                // QD accepts backrefs like \g<name> or \1 and returns str(list(groups)).
                let mut items = Vec::new();
                for backref in backrefs.iter() {
                    let item = if let Some(name) = backref.strip_prefix("\\g<") {
                        let name = name.trim_end_matches('>');
                        // QD passes the ref straight to match.group(); numeric refs
                        // resolve by index, anything else by group name.
                        if let Ok(index) = name.parse::<usize>() {
                            caps.get(index).map(|m| m.as_str()).unwrap_or("")
                        } else {
                            caps.name(name).map(|m| m.as_str()).unwrap_or("")
                        }
                    } else if let Some(index) = backref.strip_prefix('\\') {
                        caps.get(index.parse::<usize>().unwrap_or(0))
                            .map(|m| m.as_str())
                            .unwrap_or("")
                    } else {
                        return Err(Error::new(
                            ErrorKind::InvalidOperation,
                            format!("Unknown argument: {backref}"),
                        ));
                    };
                    items.push(item.to_string());
                }
                Ok::<_, Error>(JinjaValue::from(py_list_repr(&items)))
            }
        );

        qd_fn!(
            environment,
            "regex_findall",
            |value: JinjaValue, pattern: String, kwargs: Kwargs| {
                let ignorecase = kwargs
                    .get::<Option<bool>>("ignorecase")?
                    .unwrap_or_default();
                let multiline = kwargs.get::<Option<bool>>("multiline")?.unwrap_or_default();
                let re = qd_regex(&pattern, ignorecase, multiline)?;
                let subject = value.to_string();
                // Python re.findall semantics: with no groups return full matches;
                // with one group return that group; with 2+ groups return tuples.
                let group_count = re.captures_len().saturating_sub(1);
                let mut items = Vec::new();
                for caps in re.captures_iter(&subject) {
                    if group_count == 0 {
                        items.push(caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string());
                    } else if group_count == 1 {
                        items.push(caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string());
                    } else {
                        let tuple: Vec<String> = (1..=group_count)
                            .map(|i| caps.get(i).map(|m| m.as_str()).unwrap_or("").to_string())
                            .collect();
                        items.push(py_tuple_repr(&tuple));
                    }
                }
                Ok::<_, Error>(JinjaValue::from(py_list_repr(&items)))
            }
        );

        qd_fn!(environment, "regex_escape", |string: JinjaValue| {
            Ok::<_, Error>(regex::escape(&string.to_string()))
        });

        // UUID generation
        qd_fn!(
            environment,
            "to_uuid",
            |name: JinjaValue, namespace: Option<String>| {
                // Default to DNS namespace if not provided
                const DNS_NAMESPACE: &str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
                let ns_str = namespace.as_deref().unwrap_or(DNS_NAMESPACE);

                let ns_uuid = Uuid::parse_str(ns_str).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        format!("invalid namespace UUID: {e}"),
                    )
                })?;
                let name_str = name.to_string();
                Ok::<_, Error>(Uuid::new_v5(&ns_uuid, name_str.as_bytes()).to_string())
            }
        );

        // Random value generation
        environment.add_function("random_int", |min: i64, max: i64| {
            let mut rng = rand::thread_rng();
            Ok::<_, Error>(rng.gen_range(min..=max))
        });

        environment.add_function("random_float", |min: f64, max: f64| {
            let mut rng = rand::thread_rng();
            Ok::<_, Error>(rng.gen_range(min..=max))
        });

        // QD Faker (limited to the categories qdrust's fake backend supports).
        qd_fn!(environment, "Faker", |category: String| {
            fake_category(&category)
        });

        // QD random: random(1, 100, 2) -> uniform float with 2 decimals;
        // random(['a', 'b']) or random('ab') -> random element (choice).
        qd_fn!(environment, "random", |values: Rest<JinjaValue>| {
            if values.len() == 3 {
                let min = values[0].to_string().parse::<f64>().map_err(|_| {
                    Error::new(ErrorKind::InvalidOperation, "random expects numbers")
                })?;
                let max = values[1].to_string().parse::<f64>().map_err(|_| {
                    Error::new(ErrorKind::InvalidOperation, "random expects numbers")
                })?;
                let unit = values[2].to_string().parse::<i64>().map_err(|_| {
                    Error::new(ErrorKind::InvalidOperation, "random expects numbers")
                })?;
                let mut rng = rand::thread_rng();
                let picked: f64 = if max >= min {
                    rng.gen_range(min..=max)
                } else {
                    rng.gen_range(max..=min)
                };
                let precision = unit.max(0) as usize;
                Ok::<_, Error>(JinjaValue::from(format!("{picked:.precision$}")))
            } else if values.len() == 1 {
                let value = &values[0];
                if value.kind() == minijinja::value::ValueKind::String {
                    let chars: Vec<char> = value.to_string().chars().collect();
                    if chars.is_empty() {
                        return Err(Error::new(
                            ErrorKind::InvalidOperation,
                            "random choice from an empty string",
                        ));
                    }
                    let mut rng = rand::thread_rng();
                    let index = rng.gen_range(0..chars.len());
                    return Ok::<_, Error>(JinjaValue::from(chars[index].to_string()));
                }
                let items: Vec<JinjaValue> = value
                    .try_iter()
                    .map_err(|_| {
                        Error::new(ErrorKind::InvalidOperation, "random expects a sequence")
                    })?
                    .collect();
                if items.is_empty() {
                    return Err(Error::new(
                        ErrorKind::InvalidOperation,
                        "random choice from an empty sequence",
                    ));
                }
                let mut rng = rand::thread_rng();
                let index = rng.gen_range(0..items.len());
                Ok::<_, Error>(items[index].clone())
            } else {
                Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "random expects (min, max, unit) or a single sequence",
                ))
            }
        });

        // QD shuffle (randomize_list): returns a shuffled copy, optional seed.
        qd_fn!(
            environment,
            "shuffle",
            |value: JinjaValue, seed: Option<String>| {
                use rand::seq::SliceRandom;

                let items: Vec<JinjaValue> = value
                    .try_iter()
                    .map_err(|_| {
                        Error::new(ErrorKind::InvalidOperation, "shuffle expects a sequence")
                    })?
                    .collect();
                let mut items = items;
                match seed {
                    Some(seed) => {
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        std::hash::Hash::hash(&seed, &mut hasher);
                        use rand::SeedableRng;
                        let mut rng =
                            rand::rngs::StdRng::seed_from_u64(std::hash::Hasher::finish(&hasher));
                        for index in (1..items.len()).rev() {
                            let swap = rng.gen_range(0..=index);
                            items.swap(index, swap);
                        }
                    }
                    None => items.shuffle(&mut rand::thread_rng()),
                }
                Ok::<_, Error>(JinjaValue::from_iter(items))
            }
        );

        environment.add_function("fake", |category: String| fake_category(&category));

        // Utility functions
        // QD ternary: value ? true_val : false_val, with optional none_val for
        // undefined/None values.
        qd_fn!(
            environment,
            "ternary",
            |value: JinjaValue, true_val: JinjaValue, false_val: JinjaValue, kwargs: Kwargs| {
                let none_val = kwargs.get::<Option<JinjaValue>>("none_val")?;
                if let Some(none_value) =
                    none_val.filter(|_| value.is_undefined() || value.is_none())
                {
                    Ok::<_, Error>(none_value)
                } else if value.is_true() {
                    Ok::<_, Error>(true_val)
                } else {
                    Ok::<_, Error>(false_val)
                }
            }
        );

        qd_fn!(
            environment,
            "mandatory",
            |value: JinjaValue, msg: Option<String>| {
                if value.is_undefined() || value.is_none() {
                    let error_msg =
                        msg.unwrap_or_else(|| "Mandatory variable is undefined".to_string());
                    Err(Error::new(ErrorKind::UndefinedError, error_msg))
                } else {
                    Ok::<_, Error>(value)
                }
            }
        );

        qd_fn!(environment, "type_debug", |value: JinjaValue| {
            let type_name = if value.is_undefined() {
                "undefined"
            } else if value.is_none() {
                "none"
            } else if value.kind() == minijinja::value::ValueKind::Bool {
                "bool"
            } else if value.is_number() {
                if value.to_string().contains('.') {
                    "float"
                } else {
                    "int"
                }
            } else if value.kind() == minijinja::value::ValueKind::String {
                "string"
            } else if value.kind() == minijinja::value::ValueKind::Seq {
                "list"
            } else if value.kind() == minijinja::value::ValueKind::Map {
                "object"
            } else {
                "unknown"
            };
            Ok::<_, Error>(type_name)
        });

        environment.add_function("lipsum", |n: Option<i64>| {
            const LOREM_IPSUM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";

            let sentences: Vec<&str> = LOREM_IPSUM.split(". ").collect();
            let count = n.unwrap_or(1).max(1) as usize;
            let result = sentences.iter()
                .cycle()
                .take(count)
                .map(|s| s.trim())
                .collect::<Vec<_>>()
                .join(". ");

            Ok::<_, Error>(if result.ends_with('.') { result } else { format!("{}.", result) })
        });

        // String manipulation filters
        environment.add_filter("upper", |value: String| {
            Ok::<_, Error>(value.to_uppercase())
        });

        environment.add_filter("lower", |value: String| {
            Ok::<_, Error>(value.to_lowercase())
        });

        environment.add_filter("capitalize", |value: String| {
            let mut chars = value.chars();
            match chars.next() {
                None => Ok::<_, Error>(String::new()),
                Some(first) => Ok(first
                    .to_uppercase()
                    .chain(chars.as_str().to_lowercase().chars())
                    .collect()),
            }
        });

        environment.add_filter("title", |value: String| {
            let result = value
                .split_whitespace()
                .map(|word| {
                    let mut chars = word.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(first) => first.to_uppercase().chain(chars).collect(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            Ok::<_, Error>(result)
        });

        environment.add_filter("trim", |value: String| {
            Ok::<_, Error>(value.trim().to_string())
        });

        environment.add_filter("strip", |value: String| {
            Ok::<_, Error>(value.trim().to_string())
        });

        environment.add_filter("replace", |value: String, old: String, new: String| {
            Ok::<_, Error>(value.replace(&old, &new))
        });

        environment.add_filter("split", |value: String, sep: Option<String>| {
            let separator = sep.as_deref().unwrap_or(" ");
            let parts: Vec<JinjaValue> = value.split(separator).map(JinjaValue::from).collect();
            Ok::<_, Error>(JinjaValue::from_iter(parts))
        });

        environment.add_filter("join", |value: JinjaValue, sep: Option<String>| {
            let separator = sep.as_deref().unwrap_or("");
            if let Ok(iter) = value.try_iter() {
                let parts: Vec<String> = iter.map(|v| v.to_string()).collect();
                Ok::<_, Error>(parts.join(separator))
            } else {
                Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "join requires an iterable",
                ))
            }
        });

        // Collection filters
        environment.add_filter("first", |value: JinjaValue| {
            if let Ok(mut iter) = value.try_iter() {
                iter.next()
                    .ok_or_else(|| Error::new(ErrorKind::InvalidOperation, "sequence is empty"))
            } else {
                Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "first requires an iterable",
                ))
            }
        });

        environment.add_filter("last", |value: JinjaValue| {
            if let Ok(iter) = value.try_iter() {
                let items: Vec<_> = iter.collect();
                items
                    .last()
                    .cloned()
                    .ok_or_else(|| Error::new(ErrorKind::InvalidOperation, "sequence is empty"))
            } else {
                Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "last requires an iterable",
                ))
            }
        });

        environment.add_filter("reverse", |value: JinjaValue| {
            if let Ok(iter) = value.try_iter() {
                let mut items: Vec<_> = iter.collect();
                items.reverse();
                Ok::<_, Error>(JinjaValue::from_iter(items))
            } else {
                Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "reverse requires an iterable",
                ))
            }
        });

        environment.add_filter("sort", |value: JinjaValue| {
            if let Ok(iter) = value.try_iter() {
                let mut items: Vec<_> = iter.map(|v| v.to_string()).collect();
                items.sort();
                Ok::<_, Error>(JinjaValue::from_iter(items))
            } else {
                Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "sort requires an iterable",
                ))
            }
        });

        environment.add_filter("unique", |value: JinjaValue| {
            if let Ok(iter) = value.try_iter() {
                let mut seen = std::collections::HashSet::new();
                let items: Vec<JinjaValue> = iter.filter(|v| seen.insert(v.to_string())).collect();
                Ok::<_, Error>(JinjaValue::from_iter(items))
            } else {
                Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "unique requires an iterable",
                ))
            }
        });

        environment.add_filter(
            "slice",
            |value: JinjaValue, start: Option<i64>, end: Option<i64>| {
                if let Ok(iter) = value.try_iter() {
                    let items: Vec<_> = iter.collect();
                    let len = items.len() as i64;
                    let start_idx = start.unwrap_or(0).max(0) as usize;
                    let end_idx = end
                        .map(|e| if e < 0 { (len + e).max(0) } else { e })
                        .unwrap_or(len) as usize;
                    let end_idx = end_idx.min(items.len());

                    if start_idx <= end_idx {
                        Ok::<_, Error>(JinjaValue::from_iter(
                            items[start_idx..end_idx].iter().cloned(),
                        ))
                    } else {
                        Ok(JinjaValue::from_iter(Vec::<JinjaValue>::new()))
                    }
                } else {
                    Err(Error::new(
                        ErrorKind::InvalidOperation,
                        "slice requires an iterable",
                    ))
                }
            },
        );

        // JSON filters
        environment.add_filter("tojson", |value: JinjaValue| {
            serde_json::to_string(&value)
                .map_err(|e| Error::new(ErrorKind::InvalidOperation, format!("tojson failed: {e}")))
        });

        environment.add_filter("fromjson", |value: String| {
            serde_json::from_str::<serde_json::Value>(&value)
                .map(|v| JinjaValue::from_serialize(&v))
                .map_err(|e| {
                    Error::new(ErrorKind::InvalidOperation, format!("fromjson failed: {e}"))
                })
        });

        // Utility filters. Jinja2 owns `default` (QD adds nothing of its own
        // under this name), and Jinja2's third parameter is the difference that
        // makes a migrated template render: `boolean=True` also replaces a
        // falsey value, so `{{ x|default('y', boolean=True) }}` works. `d` is
        // Jinja2's alias and takes the same arguments.
        for name in ["default", "d"] {
            environment.add_filter(name, |value: JinjaValue, rest: Rest<JinjaValue>| {
                default_filter(value, &rest)
            });
        }

        environment.add_filter("abs", |value: JinjaValue| {
            let num = parse_f64(&value)?;
            Ok::<_, Error>(num.abs())
        });

        environment.add_filter("round", |value: JinjaValue, precision: Option<i32>| {
            let num = parse_f64(&value)?;
            let prec = precision.unwrap_or(0);
            if prec == 0 {
                Ok::<_, Error>(num.round())
            } else {
                let multiplier = 10f64.powi(prec);
                Ok((num * multiplier).round() / multiplier)
            }
        });

        environment.add_filter("min", |value: JinjaValue| {
            if let Ok(iter) = value.try_iter() {
                iter.map(|v| parse_f64(&v))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .ok_or_else(|| Error::new(ErrorKind::InvalidOperation, "sequence is empty"))
            } else {
                Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "min requires an iterable",
                ))
            }
        });

        environment.add_filter("max", |value: JinjaValue| {
            if let Ok(iter) = value.try_iter() {
                iter.map(|v| parse_f64(&v))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .ok_or_else(|| Error::new(ErrorKind::InvalidOperation, "sequence is empty"))
            } else {
                Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "max requires an iterable",
                ))
            }
        });

        environment.add_filter("sum", |value: JinjaValue| {
            if let Ok(iter) = value.try_iter() {
                let sum: f64 = iter
                    .map(|v| parse_f64(&v))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .sum();
                Ok::<_, Error>(sum)
            } else {
                Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "sum requires an iterable",
                ))
            }
        });

        // String test filters
        environment.add_filter("startswith", |value: String, prefix: String| {
            Ok::<_, Error>(value.starts_with(&prefix))
        });

        environment.add_filter("endswith", |value: String, suffix: String| {
            Ok::<_, Error>(value.ends_with(&suffix))
        });

        // The Python list-mutation idiom (issue #27): `{% set parts = [] %}` in
        // the template itself, then `parts.append(...)` while it is built up.
        // MiniJinja's literal `[]` is immutable and has no methods, and the
        // Jinja2 `{% set %}` scoping that makes `{% set parts = parts + [x] %}`
        // non-persistent inside `{% for %}` is exactly why QD templates use
        // `.append` in the first place — so the accumulator is handed out as a
        // mutable object instead. See `MutableSeq` / `compat_mutable_lists`.
        environment.add_function("__qd_list", |initial: Option<JinjaValue>| {
            let items = match initial {
                Some(initial) => initial
                    .try_iter()
                    .map_err(|err| {
                        Error::new(ErrorKind::InvalidOperation, format!("__qd_list: {err}"))
                    })?
                    .collect::<Vec<_>>(),
                None => Vec::new(),
            };
            Ok(JinjaValue::from_object(MutableSeq(Mutex::new(items))))
        });

        // The Python method calls (`value.split(",")`, `row.get("id")`) a QD
        // template is written with. MiniJinja has no methods on builtin values
        // at all, so `compat_python` rewrites each call into this one, which
        // dispatches on the receiver's shape at render time.
        environment.add_function(
            "__qdm_call",
            |receiver: JinjaValue, name: String, args: Rest<JinjaValue>, kwargs: Kwargs| {
                python_method(&receiver, &name, &args.0, &kwargs)
            },
        );
        // `"%s" % x` — Python's printf formatting, which Jinja2 inherits from
        // Python and MiniJinja reads as "modulo on a string" and refuses.
        environment.add_function("__qdm_percent", |template: String, values: JinjaValue| {
            format_percent(&template, &values)
        });

        // Jinja2 filters MiniJinja does not carry. QD renders through Jinja2,
        // so these are part of the surface a template may use even though no
        // QD global defines them — `{{ content|striptags }}` on the step after
        // a site's HTML page is the one issue #33 reported.
        environment.add_filter("striptags", |value: JinjaValue| {
            let text = value.to_string();
            let stripped = STRIPTAGS.replace_all(&text, "");
            Ok::<_, Error>(collapse_whitespace(&stripped))
        });

        environment.add_filter("wordcount", |value: JinjaValue| {
            Ok::<_, Error>(WORD.find_iter(&value.to_string()).count() as i64)
        });

        environment.add_filter(
            "truncate",
            |value: JinjaValue, length: Option<i64>, killwords: Option<bool>, kwargs: Kwargs| {
                let length = kwargs
                    .get::<Option<i64>>("length")?
                    .or(length)
                    .unwrap_or(255);
                let killwords = kwargs
                    .get::<Option<bool>>("killwords")?
                    .or(killwords)
                    .unwrap_or(false);
                let end = kwargs
                    .get::<Option<String>>("end")?
                    .unwrap_or_else(|| "...".to_string());
                // Jinja2's `truncate.leeway` policy: a string only five
                // characters over the limit is left alone, because cutting it
                // saves nothing worth an ellipsis.
                let leeway = kwargs.get::<Option<i64>>("leeway")?.unwrap_or(5);
                Ok::<_, Error>(truncate(
                    &value.to_string(),
                    length,
                    killwords,
                    &end,
                    leeway,
                ))
            },
        );

        environment.add_filter(
            "wordwrap",
            |value: JinjaValue,
             width: Option<i64>,
             break_long_words: Option<bool>,
             wrapstring: Option<String>,
             kwargs: Kwargs| {
                let width = kwargs.get::<Option<i64>>("width")?.or(width).unwrap_or(79);
                let break_long_words = kwargs
                    .get::<Option<bool>>("break_long_words")?
                    .or(break_long_words)
                    .unwrap_or(true);
                let wrapstring = kwargs
                    .get::<Option<String>>("wrapstring")?
                    .or(wrapstring)
                    .unwrap_or_else(|| "\n".to_string());
                Ok::<_, Error>(wordwrap(
                    &value.to_string(),
                    width.max(1) as usize,
                    break_long_words,
                    &wrapstring,
                ))
            },
        );

        environment.add_filter("center", |value: JinjaValue, width: Option<i64>| {
            Ok::<_, Error>(center(
                &value.to_string(),
                width.unwrap_or(80).max(0) as usize,
            ))
        });

        // Jinja2 takes these two flags positionally as well as by name
        // (`filesizeformat(true)`, `xmlattr(false)`), so the positional slot and
        // the keyword are both read, with the keyword winning when it is given.
        environment.add_filter(
            "filesizeformat",
            |value: JinjaValue, binary: Option<bool>, kwargs: Kwargs| {
                let binary = kwargs
                    .get::<Option<bool>>("binary")?
                    .or(binary)
                    .unwrap_or(false);
                Ok::<_, Error>(filesizeformat(&value.to_string(), binary))
            },
        );

        environment.add_filter(
            "xmlattr",
            |value: JinjaValue, autospace: Option<bool>, kwargs: Kwargs| {
                let autospace = kwargs
                    .get::<Option<bool>>("autospace")?
                    .or(autospace)
                    .unwrap_or(true);
                Ok::<_, Error>(xmlattr(&value, autospace))
            },
        );

        Self { environment }
    }
}

impl QdExpressionEngine {
    pub fn evaluate(&self, expression: &str, variables: &BTreeMap<String, Value>) -> Result<Value> {
        let source = compat_python_expression(expression);
        let compiled = self
            .environment
            .compile_expression(&source)
            .context("invalid QD expression")?;
        let value = compiled
            .eval(variables)
            .map_err(minijinja_error)
            .context("cannot evaluate QD expression")?;
        serde_json::to_value(value).context("cannot convert QD expression result")
    }

    /// Render a template string with the full QD function/filter set. Used by
    /// the server for non-template tasks so `{{ var }}` and qd functions such
    /// as `{{ md5(x) }}` work in plain task URLs, headers and bodies too.
    pub fn render(&self, template: &str, variables: &BTreeMap<String, Value>) -> Result<String> {
        self.environment
            .render_str(&compat_python_template(template), variables)
            .map_err(minijinja_error)
            .context("cannot render QD template value")
    }

    /// Every name the engine resolves on its own: the QD-compatible functions
    /// and filters (`md5`, `urlencode`, `int`, ...) plus MiniJinja's builtins
    /// (`range`, `dict`, ...).
    ///
    /// QD subtracts an equivalent list (its `libs/utils.py: jinja_globals`)
    /// when deciding which variables a template needs from the user, so that a
    /// helper call such as `{{ md5(password) }}` contributes `password` rather
    /// than `md5`.
    pub fn known_names(&self) -> HashSet<String> {
        self.environment
            .globals()
            .map(|(name, _)| name.to_string())
            .collect()
    }

    /// The variables `source` reads from the environment, in first-appearance
    /// order, minus the names the engine provides itself. This is the
    /// `jinja2.meta.find_undeclared_variables` step of QD's
    /// `HARSave.get_variables`.
    ///
    /// Filter names are not variables (`{{ x|urlencode }}` only reads `x`), and
    /// a source that is not valid Jinja yields nothing rather than an error.
    /// QD's `env.parse` swallows those too, which is what keeps `{% while ... %}`
    /// control entries from contributing bogus inputs.
    pub fn undeclared_variables(&self, source: &str) -> Vec<String> {
        let known = self.known_names();
        let undeclared = undeclared_names(source)
            .into_iter()
            .filter(|name| !known.contains(name))
            .collect::<HashSet<_>>();
        if undeclared.is_empty() {
            return Vec::new();
        }
        // `undeclared_names` is a set, so recover the source order: the variable
        // form should read like the template. Identifiers inside string literals
        // match here too, but the membership check drops them.
        let mut ordered = Vec::new();
        let mut seen = HashSet::new();
        for found in IDENTIFIER.find_iter(source) {
            let name = found.as_str();
            if undeclared.contains(name) && seen.insert(name) {
                ordered.push(name.to_string());
            }
        }
        // Safety net: a name the AST reported but the scan missed would be
        // silently dropped from the form, so keep it (never expected, because
        // MiniJinja identifiers are `[A-Za-z_][A-Za-z0-9_]*`).
        let mut leftovers = undeclared
            .into_iter()
            .filter(|name| !seen.contains(name.as_str()))
            .collect::<Vec<_>>();
        leftovers.sort();
        ordered.extend(leftovers);
        ordered
    }

    /// Literal defaults attached to a variable with `{{ name|default(...) }}`,
    /// in source order.
    ///
    /// This is the `init_env` half of QD's `HARSave.post`
    /// (`web/handlers/har.py`), which walks the parsed AST for `default`
    /// filters, requires the filter's target to be a bare name, and takes the
    /// first argument only when it is a constant. MiniJinja's AST sits behind
    /// the `unstable_machinery` feature, so this reads the source with the same
    /// restrictions: a bare name (never `a.b`), `default` applied directly to it
    /// (never after another filter, whose AST node would not be a name), and a
    /// literal first argument. A non-literal (`default(other)`) contributes
    /// nothing, exactly as `as_const()` fails there.
    pub fn declared_defaults(&self, source: &str) -> Vec<(String, String)> {
        let mut defaults = Vec::new();
        for capture in DEFAULT_FILTER.captures_iter(source) {
            let (Some(name), Some(whole)) = (capture.get(1), capture.get(0)) else {
                continue;
            };
            // `default` after another filter (`x|urlencode|default(...)`) is a
            // filter node in QD's AST, not a name, so it declares nothing. The
            // regex reads `urlencode` as the name; the `|` before it says that
            // it is itself a filter rather than the variable.
            if source[..name.start()].trim_end().ends_with('|') {
                continue;
            }
            if let Some(value) = first_literal_argument(&source[whole.end()..]) {
                defaults.push((name.as_str().to_string(), value));
            }
        }
        defaults
    }

    pub fn evaluate_bool(
        &self,
        expression: &str,
        variables: &BTreeMap<String, Value>,
    ) -> Result<bool> {
        let source = compat_python_expression(expression);
        let compiled = self
            .environment
            .compile_expression(&source)
            .context("invalid QD condition")?;
        Ok(compiled
            .eval(variables)
            .context("cannot evaluate QD condition")?
            .is_true())
    }
}

/// The two entry points the engine's own paths go through, so every rewrite
/// happens in one place: [`rewrite_template`] for a `render_str` body and
/// [`rewrite_expression`] for a bare condition or loop source.
fn compat_python_template(source: &str) -> Cow<'_, str> {
    match rewrite_template(source) {
        Some(rewritten) => Cow::Owned(rewritten),
        None => Cow::Borrowed(source),
    }
}

fn compat_python_expression(source: &str) -> Cow<'_, str> {
    match rewrite_expression(source) {
        Some(rewritten) => Cow::Owned(rewritten),
        None => Cow::Borrowed(source),
    }
}

/// Jinja identifiers, used to report discovered variables in source order.
static IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z_][A-Za-z0-9_]*").expect("identifier pattern must compile")
});

/// `name|default(` — the shape of a default a user can see in the variable form.
///
/// `regex` has no lookbehind, so the leading class consumes whatever precedes
/// the name and rejects it when that could make the name part of something else
/// (a dotted path such as `a.b|default(...)`, or a longer identifier).
static DEFAULT_FILTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^A-Za-z0-9_.])([A-Za-z_][A-Za-z0-9_]*)\s*\|\s*default\s*\(")
        .expect("default-filter pattern must compile")
});

/// The first argument of a `default(...)` call, when it is a literal.
///
/// Strings are unescaped for the escapes QD templates actually use; numbers keep
/// their source text, and booleans become `true` / `false`. Anything that is not
/// a constant (a variable, `none`, an expression) yields nothing — QD's
/// `as_const()` raises there and the default is dropped.
fn first_literal_argument(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    let quote = rest.chars().next()?;
    if quote == '\'' || quote == '"' {
        let mut value = String::new();
        let mut escaped = false;
        for ch in rest[quote.len_utf8()..].chars() {
            if escaped {
                value.push(match ch {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '\\' => '\\',
                    '\'' => '\'',
                    '"' => '"',
                    other => other,
                });
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                return Some(value);
            } else {
                value.push(ch);
            }
        }
        return None;
    }
    let token: String = rest
        .chars()
        .take_while(|ch| !ch.is_whitespace() && *ch != ',' && *ch != ')')
        .collect();
    if token == "true" || token == "false" || token.parse::<f64>().is_ok() {
        return Some(token);
    }
    None
}

/// Undeclared names in a QD template fragment.
///
/// Parsed with a bare environment, exactly like QD's `HARSave.get_variables`
/// (`Environment()` in `web/handlers/har.py`): unknown filters are fine because
/// filters are resolved at render time, while an unbalanced or unsupported tag
/// (`{% while ... %}`, a lone `{% endif %}`) is a syntax error and yields no
/// names at all.
fn undeclared_names(source: &str) -> HashSet<String> {
    let environment = Environment::new();
    let Ok(template) = environment.template_from_str(source) else {
        return HashSet::new();
    };
    template.undeclared_variables(false)
}

/// A list that Python mutation methods work on.
///
/// MiniJinja values are immutable: the `[]` a template declares is a builtin
/// sequence with no methods, and QD templates build their accumulators with
/// `parts.append(x)` — the Python idiom, and the only one available to them,
/// because Jinja2's `{% set %}` does not persist across `{% for %}` iterations
/// (verified against MiniJinja: a `{% set parts = parts + [x] %}` rewrite loses
/// every iteration but the last). When a template declares its accumulator with
/// `{% set name = [] %}` (see [`rewrite_literal_lists`]) the declaration is
/// routed here instead, and the familiar calls mutate the shared object.
/// Everywhere else — iteration, `|length`, indexing, `|join`, comparisons,
/// serialization into extracted variables — it behaves like a plain array.
#[derive(Debug, Default)]
struct MutableSeq(Mutex<Vec<JinjaValue>>);

impl Object for MutableSeq {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Seq
    }

    fn get_value(self: &Arc<Self>, key: &JinjaValue) -> Option<JinjaValue> {
        let index = key.as_usize()?;
        self.0.lock().unwrap().get(index).cloned()
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        // Owned clone: the lock guard cannot outlive the call, and QD
        // accumulators are small.
        Enumerator::Iter(Box::new(self.0.lock().unwrap().clone().into_iter()))
    }

    fn call_method(
        self: &Arc<Self>,
        _state: &State,
        name: &str,
        args: &[JinjaValue],
    ) -> Result<JinjaValue, Error> {
        let mut items = self.0.lock().unwrap();
        match name {
            "append" => {
                ensure_one_arg(name, args)?;
                items.push(args[0].clone());
                Ok(JinjaValue::UNDEFINED)
            }
            "extend" => {
                ensure_one_arg(name, args)?;
                let incoming = args[0]
                    .try_iter()
                    .map_err(|_| not_a_list(name, &args[0]))?
                    .collect::<Vec<_>>();
                items.extend(incoming);
                Ok(JinjaValue::UNDEFINED)
            }
            "insert" => {
                if args.len() != 2 {
                    return Err(argument_count(name, args.len(), 2));
                }
                let index =
                    clamp_index(args[0].to_string().parse().unwrap_or(i64::MAX), items.len());
                items.insert(index, args[1].clone());
                Ok(JinjaValue::UNDEFINED)
            }
            "pop" => {
                if args.len() > 1 {
                    return Err(argument_count(name, args.len(), 1));
                }
                let index = match args.first() {
                    Some(index) => {
                        clamp_index(index.to_string().parse().unwrap_or(i64::MAX), items.len())
                    }
                    None => items.len().wrapping_sub(1),
                };
                match items.get(index).cloned() {
                    Some(popped) => {
                        items.remove(index);
                        Ok(popped)
                    }
                    None => Err(Error::new(
                        ErrorKind::InvalidOperation,
                        "pop from empty list",
                    )),
                }
            }
            "remove" => {
                ensure_one_arg(name, args)?;
                let position = items
                    .iter()
                    .position(|item| *item == args[0])
                    .ok_or_else(|| {
                        Error::new(ErrorKind::InvalidOperation, "remove(x): x not in list")
                    })?;
                items.remove(position);
                Ok(JinjaValue::UNDEFINED)
            }
            "clear" => {
                items.clear();
                Ok(JinjaValue::UNDEFINED)
            }
            other => Err(Error::new(
                ErrorKind::UnknownMethod,
                format!(
                    "sequence has no method named {other} (mutable lists support append, extend, \
                     insert, pop, remove, clear)"
                ),
            )),
        }
    }
}

fn ensure_one_arg(name: &str, args: &[JinjaValue]) -> Result<(), Error> {
    if args.len() != 1 {
        return Err(argument_count(name, args.len(), 1));
    }
    Ok(())
}

fn argument_count(name: &str, got: usize, want: usize) -> Error {
    Error::new(
        ErrorKind::InvalidOperation,
        format!("{name}() takes {want} argument(s), got {got}"),
    )
}

fn not_a_list(name: &str, value: &JinjaValue) -> Error {
    Error::new(
        ErrorKind::InvalidOperation,
        format!("{name}() argument is not iterable: {value:?}"),
    )
}

/// Python `list.insert`/`list.pop` index clamping: negative counts from the
/// end, anything out of range saturates.
fn clamp_index(index: i64, len: usize) -> usize {
    if index < 0 {
        len.saturating_sub(index.unsigned_abs() as usize)
    } else {
        (index as usize).min(len)
    }
}

/// The Python list mutators a template may call, as they appear in source.
const MUTATOR_CALLS: &[&str] = &["append(", "extend(", "insert(", "pop(", "remove(", "clear("];

/// Rewrite self-declared literal lists into mutable ones so the Python
/// list-accumulator idiom keeps working (issue #27).
///
/// Only `{% set name = [literal] %}` declarations are touched — empty or a
/// flat literal — and only in templates that call a mutator somewhere, so a
/// template that never mutates renders exactly as before. Aliasing a variable
/// (`{% set copy = original %}`) is deliberately left alone: the copy would
/// share mutable state with the original, and `original` may be an extracted
/// value that must stay read-only. The rewrite is textual and before parsing,
/// which keeps it out of the variable-detection pass (`__qd_list` is an engine
/// global, invisible to QD's required-variables list) and out of the docs: the
/// template still reads exactly as Python QD wrote it.
///
/// Method calls on lists that came from *elsewhere* (extracted variables,
/// function returns, non-literal assignments) still fail — those are shared
/// MiniJinja values and cannot be swapped under a name the template did not
/// declare with a literal.
fn rewrite_literal_lists(source: &str) -> Cow<'_, str> {
    let looks_mutating =
        source.contains('.') && MUTATOR_CALLS.iter().any(|call| source.contains(call));
    if !looks_mutating {
        return Cow::Borrowed(source);
    }
    static SET_LIST_LITERAL: LazyLock<Regex> = LazyLock::new(|| {
        // Flat literals only: no nested brackets or braces inside, so strings
        // containing `]` are not rewritten (they fail loudly at render, as
        // they always have, rather than silently corrupting).
        Regex::new(r"\{%(-?)\s*set\s+([A-Za-z_]\w*)\s*=\s*(\[[^\[\]{}]*\])\s*(-?)%\}").unwrap()
    });
    SET_LIST_LITERAL.replace_all(source, "{%${1} set ${2} = __qd_list(${3}) ${4}%}")
}

/// Rewrite a whole QD template's Python spellings before MiniJinja parses it.
///
/// Nothing outside `{{ … }}` and `{% … %}` is touched, so a literal HTML page
/// or a JavaScript body that happens to contain `.split(` is copied through
/// byte for byte. Inside a tag the two spellings MiniJinja has no equivalent
/// for are rewritten:
///
/// * `{% set parts = [] %}` into a mutable accumulator (issue #27), because
///   MiniJinja's `[]` carries no methods and Jinja2's `{% set %}` does not
///   persist across `{% for %}` iterations — which is why QD templates build a
///   list with `.append` in the first place. See [`MutableSeq`].
/// * `value.split(",")` and `"%s" % x` into `__qdm_call` / `__qdm_percent`
///   calls, because MiniJinja has no methods on builtin values (`str` and
///   `dict` alike) and reads `%` on a string as modulo.
fn rewrite_template(source: &str) -> Option<String> {
    let declared = rewrite_literal_lists(source);
    let declared_changed = matches!(declared, Cow::Owned(_));
    let mut out = String::with_capacity(declared.len());
    let mut changed = declared_changed;
    let mut rest = declared.as_ref();
    loop {
        let Some(position) = rest.find('{') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..position]);
        let tail = &rest[position..];
        let (end, expression) = if tail.starts_with("{{") {
            ("}}", true)
        } else if tail.starts_with("{%") {
            ("%}", true)
        } else if tail.starts_with("{#") {
            ("#}", false)
        } else {
            out.push('{');
            rest = &tail[1..];
            continue;
        };
        let Some(offset) = find_delimiter(&tail[2..], end) else {
            // An unterminated tag: the template is broken, and the engine's own
            // complaint is more useful than anything guessed here.
            out.push_str(tail);
            break;
        };
        let whole = 2 + offset + end.len();
        if expression && let Some(rewritten) = rewrite_expression(&tail[2..2 + offset]) {
            out.push_str(&tail[..2]);
            out.push_str(&rewritten);
            out.push_str(end);
            changed = true;
        } else {
            out.push_str(&tail[..whole]);
        }
        rest = &tail[whole..];
    }
    changed.then_some(out)
}

/// Rewrite a bare expression — the shape `{% if … %}` conditions and `{% for … %}`
/// sources are evaluated in, which are not wrapped in delimiters of their own.
fn rewrite_expression(source: &str) -> Option<String> {
    // `%` first: its right-hand side is scanned as a whole, and the call it
    // produces is an ordinary primary for the method pass.
    let percent = rewrite_percent(source);
    let methods = rewrite_methods(percent.as_deref().unwrap_or(source));
    match (percent, methods) {
        (None, None) => None,
        (Some(percent), None) => Some(percent),
        (None, Some(methods)) => Some(methods),
        (Some(_), Some(methods)) => Some(methods),
    }
}

/// Rewrite `value.method(…)` into `__qdm_call(value, "method", …)`.
///
/// A call is only rewritten when the receiver is one this layer can name: a
/// primary expression it has just copied (an identifier chain, a string, a
/// group, or an earlier rewritten call) immediately followed by the dot. Calls
/// on anything else are left as written, so they fail with the engine's own
/// message instead of being mistranslated.
fn rewrite_methods(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    // Where the primary that just ended starts in `out`, and where it ends in
    // the input. A following `.method(…)` replaces the primary in place, and a
    // following group extends it (`f(x)`, `a[0]`), so both need its start.
    let mut primary_start: Option<usize> = None;
    let mut primary_stop: Option<usize> = None;
    let mut changed = false;
    while index < text.len() {
        let character = text[index..]
            .chars()
            .next()
            .expect("index always sits on a character boundary");
        let length = character.len_utf8();
        match character {
            '\'' | '"' => {
                let Some(end) = string_end(text, index) else {
                    out.push_str(&text[index..]);
                    return changed.then_some(out);
                };
                primary_start = Some(out.len());
                out.push_str(&text[index..end]);
                primary_stop = Some(end);
                index = end;
            }
            '(' | '[' | '{' => {
                let Some(end) = group_end(text, index) else {
                    out.push_str(&text[index..]);
                    return changed.then_some(out);
                };
                // The interior is rewritten too: `md5(content.strip())` holds a
                // call the outer scan would otherwise copy through untouched.
                let inner = rewrite_methods(&text[index + 1..end - 1]);
                changed |= inner.is_some();
                let interior = inner.as_deref().unwrap_or(&text[index + 1..end - 1]);
                let group = format!("{character}{interior}{}", &text[end - 1..end]);
                if primary_stop != Some(index) {
                    // A group on its own is a primary of its own.
                    primary_start = Some(out.len());
                }
                out.push_str(&group);
                primary_stop = Some(end);
                index = end;
            }
            '.' if primary_stop == Some(index) => {
                let name_start = index + 1;
                let name_end = identifier_end(text, name_start);
                let name = &text[name_start..name_end];
                let open = skip_spaces(text, name_end);
                if name_end > name_start
                    && PYTHON_METHODS.contains(&name)
                    && text[open..].starts_with('(')
                    && let Some(close) = group_end(text, open)
                {
                    let start = primary_start.expect("a primary ends at the dot");
                    let receiver = out[start..].to_string();
                    let args = rewrite_methods(&text[open + 1..close - 1])
                        .unwrap_or_else(|| text[open + 1..close - 1].to_string());
                    let call = match args.trim().is_empty() {
                        true => format!("__qdm_call({receiver}, \"{name}\")"),
                        false => format!("__qdm_call({receiver}, \"{name}\", {args})"),
                    };
                    // The receiver is already in `out`; the call replaces it.
                    out.replace_range(start.., &call);
                    primary_start = Some(start);
                    primary_stop = Some(close);
                    changed = true;
                    index = close;
                    continue;
                }
                if name_end > name_start {
                    // A plain attribute access extends the primary instead of
                    // starting a new one, so `a.b.c(…)` reads the method off
                    // the whole chain.
                    out.push_str(&format!(".{name}"));
                    primary_stop = Some(name_end);
                    index = name_end;
                    continue;
                }
                out.push('.');
                index += 1;
            }
            _ if character.is_alphanumeric() || character == '_' => {
                let end = identifier_end(text, index);
                primary_start = Some(out.len());
                out.push_str(&text[index..end]);
                primary_stop = Some(end);
                index = end;
            }
            _ => {
                out.push(character);
                index += length;
            }
        }
    }
    changed.then_some(out)
}

/// Rewrite a string literal's `%` formatting into `__qdm_percent(literal, …)`.
///
/// Python reads `%` on a `str` as printf formatting and on a number as modulo,
/// and Jinja2 inherits both from Python; MiniJinja reads only the modulo. Only
/// a literal left-hand side is rewritten, because that is the case where the
/// reading is known without evaluating anything — and the right-hand side is
/// cut at the end of the postfix expression, since Python's `%` binds tighter
/// than `+`.
fn rewrite_percent(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut index = 0;
    while index < text.len() {
        let character = text[index..]
            .chars()
            .next()
            .expect("index always sits on a character boundary");
        if character != '\'' && character != '"' {
            out.push(character);
            index += character.len_utf8();
            continue;
        }
        let Some(literal_end) = string_end(text, index) else {
            out.push_str(&text[index..]);
            break;
        };
        let after = skip_spaces(text, literal_end);
        let remainder = &text[after..];
        // `%%` and `%=` are not formatting, and neither is anything that is
        // not the operator.
        if !remainder.starts_with('%') || remainder.starts_with("%%") || remainder.starts_with("%=")
        {
            out.push_str(&text[index..literal_end]);
            index = literal_end;
            continue;
        }
        let Some(values_end) = primary_end(text, after + 1) else {
            out.push_str(&text[index..literal_end]);
            index = literal_end;
            continue;
        };
        out.push_str("__qdm_percent(");
        out.push_str(&text[index..literal_end]);
        out.push_str(", ");
        out.push_str(text[after + 1..values_end].trim());
        out.push(')');
        changed = true;
        index = values_end;
    }
    changed.then_some(out)
}

/// The end of the closing delimiter of a tag, skipping over string literals so
/// that `{{ "}}" }}` is one tag and not two.
fn find_delimiter(text: &str, end: &str) -> Option<usize> {
    let first = end.as_bytes()[0];
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < text.len() {
        match bytes[index] {
            b'\'' | b'"' => index = string_end(text, index)?,
            byte if byte == first && text[index..].starts_with(end) => return Some(index),
            _ => index += 1,
        }
    }
    None
}

/// The index just past the quoted string starting at `start`, backslash escapes
/// honoured. An unterminated string yields `None`.
fn string_end(text: &str, start: usize) -> Option<usize> {
    let quote = text[start..].chars().next()?;
    let mut index = start + quote.len_utf8();
    let mut escaped = false;
    for character in text[index..].chars() {
        index += character.len_utf8();
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            return Some(index);
        }
    }
    None
}

/// The index just past the `(`/`[`/`{` group starting at `start`, nesting
/// counted and string literals skipped.
fn group_end(text: &str, start: usize) -> Option<usize> {
    let open = text[start..].chars().next()?;
    let close = match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => return None,
    };
    let mut depth = 0usize;
    let mut index = start;
    while index < text.len() {
        let character = text[index..].chars().next()?;
        match character {
            '\'' | '"' => index = string_end(text, index)?,
            character if character == open => {
                depth += 1;
                index += character.len_utf8();
            }
            character if character == close => {
                depth = depth.saturating_sub(1);
                index += character.len_utf8();
                if depth == 0 {
                    return Some(index);
                }
            }
            character => index += character.len_utf8(),
        }
    }
    None
}

/// The index just past the identifier starting at `start`.
fn identifier_end(text: &str, start: usize) -> usize {
    let mut index = start;
    for character in text[start..].chars() {
        if character.is_alphanumeric() || character == '_' {
            index += character.len_utf8();
        } else {
            break;
        }
    }
    index
}

fn skip_spaces(text: &str, start: usize) -> usize {
    let mut index = start;
    for character in text[start..].chars() {
        if character.is_whitespace() {
            index += character.len_utf8();
        } else {
            break;
        }
    }
    index
}

/// The end of the postfix expression starting at or after `start`: a primary
/// (a literal, a group or a name) followed by any chain of calls, indexes and
/// attributes.
fn primary_end(text: &str, start: usize) -> Option<usize> {
    let mut index = skip_spaces(text, start);
    let character = text.get(index..)?.chars().next()?;
    index = match character {
        '\'' | '"' => string_end(text, index)?,
        '(' | '[' | '{' => group_end(text, index)?,
        _ if character.is_alphanumeric() || character == '_' => identifier_end(text, index),
        _ => return None,
    };
    loop {
        let next = skip_spaces(text, index);
        match text.get(next..).and_then(|rest| rest.chars().next()) {
            Some('(' | '[') => index = group_end(text, next)?,
            Some('.') => {
                let name_end = identifier_end(text, next + 1);
                if name_end == next + 1 {
                    return Some(index);
                }
                index = name_end;
            }
            // The end of the text (or anything that cannot continue a postfix)
            // ends the primary.
            _ => return Some(index),
        }
    }
}

/// `<!-- … -->` and `<tag …>` runs, for `striptags`.
static STRIPTAGS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)(<!--.*?-->|<[^>]*>)").expect("striptags pattern"));

/// Python's `\w+` runs, for `wordcount`.
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\w+").expect("word pattern"));

/// Jinja2's `striptags`: remove comments and tags, then read what is left as
/// words — every run of whitespace becomes a single space.
fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Jinja2's `truncate`. `length` counts the ellipsis, `leeway` is the slack the
/// `truncate.leeway` policy grants before anything is cut, and `killwords`
/// decides whether a word may be cut in half — Jinja2 otherwise backs up to the
/// last whole word.
fn truncate(value: &str, length: i64, killwords: bool, end: &str, leeway: i64) -> String {
    let length = length.max(0) as usize;
    let leeway = leeway.max(0) as usize;
    let characters: Vec<char> = value.chars().collect();
    if characters.len() <= length + leeway {
        return value.to_string();
    }
    let keep = length
        .saturating_sub(end.chars().count())
        .min(characters.len());
    let head: String = characters[..keep].iter().collect();
    let head = if killwords {
        head
    } else {
        match head.rsplit_once(' ') {
            Some((before, _)) => before.to_string(),
            None => head,
        }
    };
    format!("{head}{end}")
}

/// Jinja2's `wordwrap`: a greedy wrap on whitespace, with a word longer than
/// the width broken when `break_long_words` allows it.
fn wordwrap(value: &str, width: usize, break_long_words: bool, wrapstring: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in value.split_whitespace() {
        let mut word = word.to_string();
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut current));
        }
        while break_long_words && word.chars().count() > width {
            let split = word
                .char_indices()
                .nth(width)
                .map(|(index, _)| index)
                .unwrap_or(word.len());
            lines.push(word[..split].to_string());
            word = word[split..].to_string();
        }
        if current.is_empty() {
            current = word;
        } else {
            current.push(' ');
            current.push_str(&word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines.join(wrapstring)
}

/// Python's `str.center`: the odd character of padding goes on the right.
fn center(value: &str, width: usize) -> String {
    let length = value.chars().count();
    if length >= width {
        return value.to_string();
    }
    let padding = width - length;
    let left = padding / 2;
    format!("{}{value}{}", " ".repeat(left), " ".repeat(padding - left))
}

/// Jinja2's `filesizeformat`, unit table included.
fn filesizeformat(value: &str, binary: bool) -> String {
    let bytes: f64 = value.trim().parse().unwrap_or_default();
    let base = if binary { 1024.0 } else { 1000.0 };
    let prefixes = if binary {
        ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB", "ZiB", "YiB"]
    } else {
        ["kB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"]
    };
    if bytes == 1.0 {
        return "1 Byte".to_string();
    }
    if bytes < base {
        return format!("{} Bytes", bytes as i64);
    }
    let mut prefix = prefixes[0];
    for (index, candidate) in prefixes.iter().enumerate() {
        let unit = base.powi(index as i32 + 2);
        prefix = *candidate;
        if bytes < unit {
            return format!("{:.1} {candidate}", base * bytes / unit);
        }
    }
    let unit = base.powi(prefixes.len() as i32 + 1);
    format!("{:.1} {prefix}", base * bytes / unit)
}

/// Jinja2's `xmlattr`: `key="value"` pairs, `None` and undefined entries
/// dropped, and a leading space unless `autospace` is off.
fn xmlattr(value: &JinjaValue, autospace: bool) -> String {
    let Ok(keys) = value.try_iter() else {
        return String::new();
    };
    let rendered: Vec<String> = keys
        .filter_map(|key| {
            let entry = value.get_item(&key).ok()?;
            if entry.is_none() || entry.is_undefined() {
                return None;
            }
            Some(format!(
                "{}=\"{}\"",
                key,
                escape_attribute(&entry.to_string())
            ))
        })
        .collect();
    if rendered.is_empty() {
        return String::new();
    }
    let attributes = rendered.join(" ");
    if autospace {
        format!(" {attributes}")
    } else {
        attributes
    }
}

/// The escaping markupsafe's `escape` writes for an attribute value.
fn escape_attribute(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// The Python method calls the compatibility layer maps onto engine calls.
///
/// MiniJinja has no methods on builtin values at all — a `str` or a `dict`
/// answers "unknown method" — while a QD template is Python, so
/// [`compat_python`] rewrites each call into `__qdm_call` before parsing. The
/// list is the set QD templates reach for; a name outside it is left as written
/// and fails loudly rather than quietly reading as a key lookup.
///
/// The list mutators (`append`, `sort`, ...) are deliberately absent: those
/// need a receiver that can actually be written to, which is what
/// [`MutableSeq`] provides for a template's own `{% set x = [] %}`.
const PYTHON_METHODS: &[&str] = &[
    // str
    "capitalize",
    "casefold",
    "center",
    "count",
    "endswith",
    "expandtabs",
    "find",
    "format",
    "index",
    "isalnum",
    "isalpha",
    "isdecimal",
    "isdigit",
    "islower",
    "isnumeric",
    "isspace",
    "istitle",
    "isupper",
    "join",
    "ljust",
    "lower",
    "lstrip",
    "partition",
    "removeprefix",
    "removesuffix",
    "replace",
    "rfind",
    "rindex",
    "rjust",
    "rpartition",
    "rsplit",
    "rstrip",
    "split",
    "splitlines",
    "startswith",
    "strip",
    "swapcase",
    "title",
    "upper",
    "zfill",
    // dict
    "copy",
    "get",
    "items",
    "keys",
    "setdefault",
    "values",
    // list / tuple
    "index",
    "count",
];

/// Dispatch one rewritten Python method call.
///
/// The receiver's shape decides what a name means: `get`/`keys`/`items` are a
/// mapping's, `index`/`count` a sequence's, and everything else reads the
/// receiver as text — which is what a QD variable is, since extraction only
/// ever stores strings and JSON.
fn python_method(
    receiver: &JinjaValue,
    name: &str,
    args: &[JinjaValue],
    kwargs: &Kwargs,
) -> Result<JinjaValue, Error> {
    if receiver.kind() == ValueKind::Map
        && let Some(result) = mapping_method(receiver, name, args)
    {
        return result;
    }
    if matches!(receiver.kind(), ValueKind::Seq | ValueKind::Iterable)
        && let Some(result) = sequence_method(receiver, name, args)
    {
        return result;
    }
    match string_method(receiver, name, args, kwargs) {
        Some(result) => result,
        None => Err(Error::new(
            ErrorKind::UnknownMethod,
            format!("{} has no method named {name}", receiver.kind()),
        )),
    }
}

/// A `dict` method, or `None` when `name` is not one.
fn mapping_method(
    receiver: &JinjaValue,
    name: &str,
    args: &[JinjaValue],
) -> Option<Result<JinjaValue, Error>> {
    let result = match name {
        "get" | "setdefault" => {
            let Some(key) = args.first() else {
                return Some(Err(argument_count(name, 0, 1)));
            };
            // Python's `setdefault` also *stores* the default; a mapping that
            // arrived as an extracted variable cannot be written to here, so
            // the value the expression reads is what comes back.
            match receiver.get_item(key) {
                Ok(value) if !value.is_undefined() => Ok(value),
                _ => Ok(args.get(1).cloned().unwrap_or(JinjaValue::from(()))),
            }
        }
        "keys" => Ok(JinjaValue::from_iter(receiver.try_iter().ok()?)),
        "values" => {
            let keys: Vec<JinjaValue> = receiver.try_iter().ok()?.collect();
            Ok(JinjaValue::from_iter(
                keys.iter().filter_map(|key| receiver.get_item(key).ok()),
            ))
        }
        "items" => {
            let keys: Vec<JinjaValue> = receiver.try_iter().ok()?.collect();
            Ok(JinjaValue::from_iter(keys.iter().filter_map(|key| {
                receiver
                    .get_item(key)
                    .ok()
                    .map(|value| JinjaValue::from_iter([key.clone(), value]))
            })))
        }
        "copy" => Ok(receiver.clone()),
        _ => return None,
    };
    Some(result)
}

/// A list/tuple method, or `None` when `name` is not one.
fn sequence_method(
    receiver: &JinjaValue,
    name: &str,
    args: &[JinjaValue],
) -> Option<Result<JinjaValue, Error>> {
    let items: Vec<JinjaValue> = receiver.try_iter().ok()?.collect();
    let needle = || {
        args.first()
            .cloned()
            .ok_or_else(|| argument_count(name, 0, 1))
    };
    let result = match name {
        "index" => match needle() {
            Err(error) => Err(error),
            Ok(needle) => match items.iter().position(|item| *item == needle) {
                Some(position) => Ok(JinjaValue::from(position as i64)),
                None => Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "list.index(x): x not in list",
                )),
            },
        },
        "count" => match needle() {
            Err(error) => Err(error),
            Ok(needle) => Ok(JinjaValue::from(
                items.iter().filter(|item| **item == needle).count() as i64,
            )),
        },
        "copy" => Ok(JinjaValue::from_iter(items)),
        _ => return None,
    };
    Some(result)
}

/// A `str` method, or `None` when `name` is not one this layer reads.
fn string_method(
    receiver: &JinjaValue,
    name: &str,
    args: &[JinjaValue],
    kwargs: &Kwargs,
) -> Option<Result<JinjaValue, Error>> {
    let text = receiver.to_string();
    let at = |index: usize| args.get(index);
    let result = match name {
        "upper" => Ok(JinjaValue::from(text.to_uppercase())),
        "lower" | "casefold" => Ok(JinjaValue::from(text.to_lowercase())),
        "capitalize" => {
            let mut characters = text.chars();
            Ok(JinjaValue::from(match characters.next() {
                None => String::new(),
                Some(first) => first
                    .to_uppercase()
                    .chain(characters.as_str().to_lowercase().chars())
                    .collect::<String>(),
            }))
        }
        "title" => Ok(JinjaValue::from(
            text.split_whitespace()
                .map(|word| {
                    let mut characters = word.chars();
                    match characters.next() {
                        None => String::new(),
                        Some(first) => first.to_uppercase().chain(characters).collect::<String>(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        )),
        "swapcase" => Ok(JinjaValue::from(
            text.chars()
                .flat_map(|character| {
                    if character.is_uppercase() {
                        character.to_lowercase().collect::<Vec<_>>()
                    } else {
                        character.to_uppercase().collect::<Vec<_>>()
                    }
                })
                .collect::<String>(),
        )),
        "strip" | "lstrip" | "rstrip" => {
            let characters = at(0).map(JinjaValue::to_string);
            let trimmed = match (name, characters) {
                ("strip", Some(chars)) => text.trim_matches(|c| chars.contains(c)),
                ("strip", None) => text.trim(),
                ("lstrip", Some(chars)) => text.trim_start_matches(|c| chars.contains(c)),
                ("lstrip", None) => text.trim_start(),
                (_, Some(chars)) => text.trim_end_matches(|c| chars.contains(c)),
                (_, None) => text.trim_end(),
            };
            Ok(JinjaValue::from(trimmed))
        }
        "split" | "rsplit" => {
            let separator = at(0).map(JinjaValue::to_string);
            let maxsplit = at(1).and_then(int_argument);
            match python_split(&text, separator.as_deref(), maxsplit, name == "rsplit") {
                Ok(parts) => Ok(JinjaValue::from_iter(parts)),
                Err(error) => Err(error),
            }
        }
        "splitlines" => {
            let keepends = at(0).is_some_and(JinjaValue::is_true);
            Ok(JinjaValue::from_iter(
                python_splitlines(&text, keepends)
                    .into_iter()
                    .map(JinjaValue::from),
            ))
        }
        "replace" => {
            let (Some(old), Some(new)) = (at(0), at(1)) else {
                return Some(Err(argument_count("replace", args.len(), 2)));
            };
            let count = at(2).and_then(int_argument);
            let old = old.to_string();
            let new = new.to_string();
            Ok(JinjaValue::from(match count {
                Some(count) if count >= 0 => text.replacen(&old, &new, count as usize),
                _ => text.replace(&old, &new),
            }))
        }
        "join" => {
            let Some(iterable) = at(0) else {
                return Some(Err(argument_count("join", 0, 1)));
            };
            match iterable.try_iter() {
                Ok(items) => Ok(JinjaValue::from(
                    items
                        .map(|item| item.to_string())
                        .collect::<Vec<_>>()
                        .join(&text),
                )),
                Err(_) => Err(not_a_list("join", iterable)),
            }
        }
        "startswith" | "endswith" => {
            let Some(prefix) = at(0) else {
                return Some(Err(argument_count(name, 0, 1)));
            };
            let window = char_slice(
                &text,
                at(1).and_then(int_argument),
                at(2).and_then(int_argument),
            );
            let candidates: Vec<String> = match prefix.try_iter() {
                Ok(items) => items.map(|item| item.to_string()).collect(),
                Err(_) => vec![prefix.to_string()],
            };
            let matched = candidates.iter().any(|candidate| {
                if name == "startswith" {
                    window.starts_with(candidate.as_str())
                } else {
                    window.ends_with(candidate.as_str())
                }
            });
            Ok(JinjaValue::from(matched))
        }
        "find" | "rfind" | "index" | "rindex" => {
            let Some(needle) = at(0) else {
                return Some(Err(argument_count(name, 0, 1)));
            };
            let start = at(1).and_then(int_argument);
            let end = at(2).and_then(int_argument);
            let window = char_slice(&text, start, end);
            // Python reports the match's index in the whole string, so the
            // window's own offset is added back — and with no `start` that
            // offset is zero, not the length of the whole text.
            let offset = resolve_bound(text.chars().count() as i64, start, 0);
            let needle = needle.to_string();
            let found = if name.starts_with('r') {
                window.rfind(&needle)
            } else {
                window.find(&needle)
            };
            match found {
                Some(index) => Ok(JinjaValue::from(
                    offset + window[..index].chars().count() as i64,
                )),
                None if name.ends_with("index") => Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "substring not found",
                )),
                None => Ok(JinjaValue::from(-1i64)),
            }
        }
        "count" => {
            let Some(needle) = at(0) else {
                return Some(Err(argument_count("count", 0, 1)));
            };
            let block = char_slice(
                &text,
                at(1).and_then(int_argument),
                at(2).and_then(int_argument),
            );
            Ok(JinjaValue::from(
                block.matches(&needle.to_string()).count() as i64
            ))
        }
        "zfill" | "ljust" | "rjust" | "center" => {
            let Some(width) = at(0).and_then(int_argument) else {
                return Some(Err(argument_count(name, args.len(), 1)));
            };
            let fill = at(1)
                .map(JinjaValue::to_string)
                .and_then(|value| value.chars().next())
                .unwrap_or(' ');
            Ok(JinjaValue::from(pad(&text, width, fill, name)))
        }
        "format" => return Some(python_str_format(&text, args, kwargs)),
        "removeprefix" => {
            let Some(prefix) = at(0) else {
                return Some(Err(argument_count("removeprefix", 0, 1)));
            };
            let prefix = prefix.to_string();
            Ok(JinjaValue::from(
                text.strip_prefix(&prefix).unwrap_or(&text).to_string(),
            ))
        }
        "removesuffix" => {
            let Some(suffix) = at(0) else {
                return Some(Err(argument_count("removesuffix", 0, 1)));
            };
            let suffix = suffix.to_string();
            Ok(JinjaValue::from(
                text.strip_suffix(&suffix).unwrap_or(&text).to_string(),
            ))
        }
        "partition" | "rpartition" => {
            let Some(separator) = at(0) else {
                return Some(Err(argument_count(name, 0, 1)));
            };
            let separator = separator.to_string();
            let split = if name == "partition" {
                text.find(&separator)
            } else {
                text.rfind(&separator)
            };
            let parts = match split {
                Some(index) => vec![
                    text[..index].to_string(),
                    separator.clone(),
                    text[index + separator.len()..].to_string(),
                ],
                None if name == "partition" => vec![text.clone(), String::new(), String::new()],
                None => vec![String::new(), String::new(), text.clone()],
            };
            Ok(JinjaValue::from_iter(parts))
        }
        "expandtabs" => {
            let size = at(0).and_then(int_argument).unwrap_or(8).max(0) as usize;
            let mut out = String::with_capacity(text.len());
            let mut column = 0usize;
            for character in text.chars() {
                match character {
                    '\t' if size > 0 => {
                        let padding = size - column % size;
                        out.push_str(&" ".repeat(padding));
                        column += padding;
                    }
                    '\t' => column += 1,
                    '\n' | '\r' => {
                        out.push(character);
                        column = 0;
                    }
                    other => {
                        out.push(other);
                        column += 1;
                    }
                }
            }
            Ok(JinjaValue::from(out))
        }
        // Rust's `char` predicates are the Unicode readings Python's are
        // modelled on; they agree with Python on every character a template
        // could be testing.
        "isdigit" | "isdecimal" | "isnumeric" => Ok(JinjaValue::from(
            !text.is_empty() && text.chars().all(|character| character.is_numeric()),
        )),
        "isalpha" => Ok(JinjaValue::from(
            !text.is_empty() && text.chars().all(char::is_alphabetic),
        )),
        "isalnum" => Ok(JinjaValue::from(
            !text.is_empty() && text.chars().all(char::is_alphanumeric),
        )),
        "isspace" => Ok(JinjaValue::from(
            !text.is_empty() && text.chars().all(char::is_whitespace),
        )),
        "islower" => Ok(JinjaValue::from(
            text.chars().any(char::is_lowercase) && !text.chars().any(char::is_uppercase),
        )),
        "isupper" => Ok(JinjaValue::from(
            text.chars().any(char::is_uppercase) && !text.chars().any(char::is_lowercase),
        )),
        "istitle" => Ok(JinjaValue::from(
            !text.is_empty()
                && text
                    .split_whitespace()
                    .all(|word| word.chars().next().is_some_and(char::is_uppercase)),
        )),
        _ => return None,
    };
    Some(result)
}

/// Python's `str.split`/`str.rsplit`, including the no-separator form: runs of
/// whitespace are the separators and the empty strings between them are
/// dropped, which is what `"a  b".split()` returns and `"a  b".split(" ")` does
/// not.
fn python_split(
    text: &str,
    separator: Option<&str>,
    maxsplit: Option<i64>,
    from_right: bool,
) -> Result<Vec<JinjaValue>, Error> {
    let maxsplit = maxsplit
        .filter(|maxsplit| *maxsplit >= 0)
        .map(|maxsplit| maxsplit as usize);
    let parts: Vec<String> = match separator {
        Some("") => {
            return Err(Error::new(ErrorKind::InvalidOperation, "empty separator"));
        }
        Some(separator) => match (maxsplit, from_right) {
            (Some(maxsplit), false) => text
                .splitn(maxsplit + 1, separator)
                .map(str::to_string)
                .collect(),
            (Some(maxsplit), true) => {
                let mut parts: Vec<String> = text
                    .rsplitn(maxsplit + 1, separator)
                    .map(str::to_string)
                    .collect();
                parts.reverse();
                parts
            }
            (None, false) => text.split(separator).map(str::to_string).collect(),
            (None, true) => text.rsplit(separator).map(str::to_string).collect(),
        },
        None => match (maxsplit, from_right) {
            // Python strips the leading whitespace first, so the first split
            // lands on real content rather than on the run before it.
            (Some(0), _) => vec![text.trim_start().to_string()],
            (Some(maxsplit), false) => {
                let mut parts = Vec::new();
                let mut rest = text.trim_start();
                while parts.len() < maxsplit {
                    let trimmed = rest.trim_start();
                    let Some(index) = trimmed.find(char::is_whitespace) else {
                        rest = trimmed;
                        break;
                    };
                    parts.push(trimmed[..index].to_string());
                    rest = &trimmed[index..];
                }
                parts.push(rest.trim().to_string());
                parts
            }
            (Some(maxsplit), true) => {
                let mut parts: Vec<String> = text
                    .trim_end()
                    .rsplitn(maxsplit + 1, char::is_whitespace)
                    .filter(|part| !part.is_empty())
                    .map(str::to_string)
                    .collect();
                parts.reverse();
                parts
            }
            (None, _) => text.split_whitespace().map(str::to_string).collect(),
        },
    };
    Ok(parts.into_iter().map(JinjaValue::from).collect())
}

/// Python's `str.splitlines`: the Unicode line boundaries, with the break kept
/// when `keepends` asks for it.
fn python_splitlines(text: &str, keepends: bool) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if !is_line_boundary(character) {
            current.push(character);
            continue;
        }
        let mut boundary = character.to_string();
        if character == '\r' && characters.peek() == Some(&'\n') {
            characters.next();
            boundary.push('\n');
        }
        if keepends {
            current.push_str(&boundary);
            lines.push(std::mem::take(&mut current));
        } else {
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// The characters Python's `str.splitlines` treats as a line break.
fn is_line_boundary(character: char) -> bool {
    matches!(
        character,
        '\n' | '\r'
            | '\u{b}'
            | '\u{c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// Python's `str.zfill`/`ljust`/`rjust`/`center`, all of which pad to `width`
/// characters and never truncate.
fn pad(text: &str, width: i64, fill: char, kind: &str) -> String {
    let length = text.chars().count() as i64;
    if width <= length {
        return text.to_string();
    }
    let padding = (width - length) as usize;
    match kind {
        "ljust" => format!("{text}{}", fill.to_string().repeat(padding)),
        "rjust" => format!("{}{text}", fill.to_string().repeat(padding)),
        "center" => {
            let left = padding / 2;
            format!(
                "{}{text}{}",
                fill.to_string().repeat(left),
                fill.to_string().repeat(padding - left)
            )
        }
        // `zfill` puts the zeros after the sign, and pads with a zero whatever
        // the fill character is.
        _ => match text.strip_prefix(['-', '+']) {
            Some(rest) => format!("{}{}{rest}", &text[..1], "0".repeat(padding)),
            None => format!("{}{text}", "0".repeat(padding)),
        },
    }
}

/// Python's `str.format`: `{}`/`{0}`/`{name}` fields with an optional
/// `!conversion` and `:spec`, and `{{`/`}}` for literal braces.
fn python_str_format(
    template: &str,
    args: &[JinjaValue],
    kwargs: &Kwargs,
) -> Result<JinjaValue, Error> {
    let mut out = String::with_capacity(template.len());
    let mut characters = template.chars().peekable();
    // Python keeps automatic and manual numbering separate and refuses to mix
    // them; tracking the next automatic index is enough to match the field
    // resolution templates rely on.
    let mut automatic = 0usize;
    while let Some(character) = characters.next() {
        match character {
            '{' if characters.peek() == Some(&'{') => {
                characters.next();
                out.push('{');
            }
            '}' if characters.peek() == Some(&'}') => {
                characters.next();
                out.push('}');
            }
            '{' => {
                let mut field = String::new();
                let mut closed = false;
                for character in characters.by_ref() {
                    if character == '}' {
                        closed = true;
                        break;
                    }
                    field.push(character);
                }
                let unmatched = || {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        "expected '}' before end of string",
                    )
                };
                if !closed {
                    return Err(unmatched());
                }
                out.push_str(&format_field(&field, args, kwargs, &mut automatic)?);
            }
            '}' => {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "single '}' encountered in format string",
                ));
            }
            other => out.push(other),
        }
    }
    Ok(JinjaValue::from(out))
}

/// One `{…}` replacement field of `str.format`: `[name][!conversion][:spec]`.
fn format_field(
    field: &str,
    args: &[JinjaValue],
    kwargs: &Kwargs,
    automatic: &mut usize,
) -> Result<String, Error> {
    let (field, spec) = match field.split_once(':') {
        Some((field, spec)) => (field, Some(spec)),
        None => (field, None),
    };
    let (field, conversion) = match field.split_once('!') {
        Some((field, conversion)) => (field, Some(conversion.to_string())),
        None => (field, None),
    };
    let name_end = field.find(['.', '[']).unwrap_or(field.len());
    let (name, mut accessors) = field.split_at(name_end);
    let mut value = if name.is_empty() {
        let index = *automatic;
        *automatic += 1;
        args.get(index).cloned().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                format!("Replacement index {index} out of range for positional args tuple"),
            )
        })?
    } else if let Ok(index) = name.parse::<usize>() {
        *automatic = index + 1;
        args.get(index).cloned().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                format!("Replacement index {index} out of range for positional args tuple"),
            )
        })?
    } else {
        kwargs
            .get::<Option<JinjaValue>>(name)?
            .ok_or_else(|| Error::new(ErrorKind::InvalidOperation, format!("KeyError: {name:?}")))?
    };
    while !accessors.is_empty() {
        if let Some(rest) = accessors.strip_prefix('.') {
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            let key = JinjaValue::from(&rest[..end]);
            value = require_item(&value, &key, &format!("attribute {:?}", &rest[..end]))?;
            accessors = &rest[end..];
            continue;
        }
        let rest = accessors
            .strip_prefix('[')
            .ok_or_else(|| Error::new(ErrorKind::InvalidOperation, "invalid format field"))?;
        let end = rest.find(']').ok_or_else(|| {
            Error::new(ErrorKind::InvalidOperation, "unclosed '[' in format field")
        })?;
        let key = rest[..end].trim_matches(['\'', '"']);
        let key = match key.parse::<usize>() {
            Ok(index) => JinjaValue::from(index as i64),
            Err(_) => JinjaValue::from(key),
        };
        value = require_item(&value, &key, &format!("KeyError: {:?}", &rest[..end]))?;
        accessors = &rest[end + 1..];
    }
    if matches!(conversion.as_deref(), Some("r" | "a")) {
        return Ok(python_repr(&value));
    }
    match spec {
        Some(spec) if !spec.is_empty() => python_format(&value, spec),
        _ => Ok(value.to_string()),
    }
}

/// Python's `repr`, for `%r` and the `!r` conversion: strings are quoted (and
/// escaped) where every other kind renders as its own text.
fn python_repr(value: &JinjaValue) -> String {
    match value.kind() {
        ValueKind::String => {
            let escaped = value.to_string().replace('\\', "\\\\").replace('\'', "\\'");
            format!("'{escaped}'")
        }
        ValueKind::None => "None".to_string(),
        ValueKind::Bool => if value.is_true() { "True" } else { "False" }.to_string(),
        ValueKind::Seq | ValueKind::Iterable => {
            let items: Vec<String> = value
                .try_iter()
                .map(|items| items.map(|item| python_repr(&item)).collect())
                .unwrap_or_default();
            format!("[{}]", items.join(", "))
        }
        _ => value.to_string(),
    }
}

/// Python's printf-style formatting: `template % values`, over a single value,
/// a tuple/list of them or a mapping (`%(name)s`).
fn format_percent(template: &str, values: &JinjaValue) -> Result<String, Error> {
    let mapping = (values.kind() == ValueKind::Map).then_some(values);
    let positional: Vec<JinjaValue> = match mapping {
        Some(_) => Vec::new(),
        None => match values.kind() {
            ValueKind::Seq | ValueKind::Iterable => values
                .try_iter()
                .map_err(|err| Error::new(ErrorKind::InvalidOperation, err.to_string()))?
                .collect(),
            _ => vec![values.clone()],
        },
    };

    let mut next = 0usize;
    let mut out = String::with_capacity(template.len());
    let mut characters = template.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '%' {
            out.push(character);
            continue;
        }
        if characters.peek() == Some(&'%') {
            characters.next();
            out.push('%');
            continue;
        }
        if characters.peek() == Some(&'(') {
            characters.next();
            let mut name = String::new();
            for character in characters.by_ref() {
                if character == ')' {
                    break;
                }
                name.push(character);
            }
            let Some(mapping) = mapping else {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "format requires a mapping",
                ));
            };
            let value = require_item(
                mapping,
                &JinjaValue::from(name.as_str()),
                &format!("KeyError: {name:?}"),
            )?;
            let (flags, width, precision, kind) = read_conversion(&mut characters)?;
            out.push_str(&percent_conversion(kind, &value, &flags, width, precision)?);
            continue;
        }
        let (flags, width, precision, kind) = read_conversion(&mut characters)?;
        let Some(value) = positional.get(next).cloned() else {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                "not enough arguments for format string",
            ));
        };
        next += 1;
        out.push_str(&percent_conversion(kind, &value, &flags, width, precision)?);
    }
    Ok(out)
}

/// The flags, width, precision and conversion character of one `%` conversion.
fn read_conversion<I: Iterator<Item = char>>(
    characters: &mut std::iter::Peekable<I>,
) -> Result<(String, Option<usize>, Option<usize>, char), Error> {
    let mut flags = String::new();
    while matches!(
        characters.peek().copied(),
        Some('-' | '+' | ' ' | '0' | '#')
    ) {
        flags.push(characters.next().expect("peeked"));
    }
    let mut width = String::new();
    while characters.peek().is_some_and(char::is_ascii_digit) {
        width.push(characters.next().expect("peeked"));
    }
    let mut precision = None;
    if characters.peek() == Some(&'.') {
        characters.next();
        let mut digits = String::new();
        while characters.peek().is_some_and(char::is_ascii_digit) {
            digits.push(characters.next().expect("peeked"));
        }
        precision = Some(digits.parse::<usize>().unwrap_or(0));
    }
    let Some(kind) = characters.next() else {
        return Err(Error::new(ErrorKind::InvalidOperation, "incomplete format"));
    };
    Ok((flags, width.parse::<usize>().ok(), precision, kind))
}

/// One printf conversion, rendered and padded.
fn percent_conversion(
    kind: char,
    value: &JinjaValue,
    flags: &str,
    width: Option<usize>,
    precision: Option<usize>,
) -> Result<String, Error> {
    let integer = || -> Result<i64, Error> {
        let text = value.to_string();
        text.parse::<i64>()
            .or_else(|_| text.parse::<f64>().map(|number| number.trunc() as i64))
            .map_err(|_| {
                Error::new(
                    ErrorKind::InvalidOperation,
                    "a number is required for this conversion",
                )
            })
    };
    let float = || -> Result<f64, Error> {
        value.to_string().parse::<f64>().map_err(|_| {
            Error::new(
                ErrorKind::InvalidOperation,
                "a number is required for this conversion",
            )
        })
    };
    let mut body = match kind {
        's' => match precision {
            Some(precision) => value.to_string().chars().take(precision).collect(),
            None => value.to_string(),
        },
        'r' | 'a' => python_repr(value),
        'd' | 'i' | 'u' => integer()?.to_string(),
        'f' | 'F' => format!("{:.*}", precision.unwrap_or(6), float()?),
        'e' => format!("{:.*e}", precision.unwrap_or(6), float()?),
        'E' => format!("{:.*E}", precision.unwrap_or(6), float()?),
        'g' | 'G' => {
            let rendered = general_float(float()?, precision.unwrap_or(6));
            if kind == 'G' {
                rendered.to_uppercase()
            } else {
                rendered
            }
        }
        'x' => format!("{:x}", integer()?),
        'X' => format!("{:X}", integer()?),
        'o' => format!("{:o}", integer()?),
        'c' => match value.kind() {
            ValueKind::Number => char::from_u32(integer()?.max(0) as u32)
                .map(|character| character.to_string())
                .ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "not a valid character code")
                })?,
            _ => value
                .to_string()
                .chars()
                .next()
                .map(|character| character.to_string())
                .unwrap_or_default(),
        },
        other => {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                format!("unsupported format character {other:?}"),
            ));
        }
    };

    let length = body.chars().count();
    if let Some(width) = width.filter(|width| *width > length) {
        let padding = width - length;
        if flags.contains('-') {
            body.push_str(&" ".repeat(padding));
        } else if flags.contains('0')
            && matches!(
                kind,
                'd' | 'i' | 'u' | 'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'x' | 'X' | 'o'
            )
        {
            // A zero-padded number keeps its sign in front of the zeros.
            body = match body.strip_prefix(['-', '+']) {
                Some(rest) => format!("{}{}{rest}", &body[..1], "0".repeat(padding)),
                None => format!("{}{body}", "0".repeat(padding)),
            };
        } else {
            body = format!("{}{body}", " ".repeat(padding));
        }
    }
    Ok(body)
}

/// Python's `%g`: the shorter of the exponent and decimal forms, with trailing
/// zeros removed.
fn general_float(number: f64, precision: usize) -> String {
    if number == 0.0 {
        return "0".to_string();
    }
    let precision = precision.max(1);
    let exponent = number.abs().log10().floor() as i32;
    let rendered = if exponent < -4 || exponent >= precision as i32 {
        format!("{:.*e}", precision - 1, number)
    } else {
        format!(
            "{:.*}",
            (precision as i32 - 1 - exponent).max(0) as usize,
            number
        )
    };
    match rendered.find(['e', 'E']) {
        Some(exponent_at) => {
            let (mantissa, exponent_part) = rendered.split_at(exponent_at);
            format!(
                "{}{exponent_part}",
                mantissa.trim_end_matches('0').trim_end_matches('.')
            )
        }
        None => rendered
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string(),
    }
}

/// Look a key up on a value the way Python's `[]` and attribute access do: a
/// missing key is an error rather than an undefined value that would render as
/// nothing and hide the mistake.
fn require_item(value: &JinjaValue, key: &JinjaValue, what: &str) -> Result<JinjaValue, Error> {
    match value.get_item(key) {
        Ok(resolved) if !resolved.is_undefined() => Ok(resolved),
        _ => Err(Error::new(
            ErrorKind::InvalidOperation,
            format!("{what} is not defined"),
        )),
    }
}

/// Python's `s[start:end]`: negative bounds count from the end, out-of-range
/// bounds saturate, and a missing bound is the corresponding end of the string.
/// Python's bound resolution for a slice: a negative bound counts from the end
/// and is clamped at zero, a positive one is clamped at the length, and a
/// missing one takes the fallback.
fn resolve_bound(length: i64, bound: Option<i64>, fallback: i64) -> i64 {
    match bound {
        Some(bound) if bound < 0 => (length + bound).max(0),
        Some(bound) => bound.min(length),
        None => fallback,
    }
}

fn char_slice(text: &str, start: Option<i64>, end: Option<i64>) -> String {
    let characters: Vec<char> = text.chars().collect();
    let length = characters.len() as i64;
    let start = resolve_bound(length, start, 0);
    let end = resolve_bound(length, end, length);
    if end <= start {
        return String::new();
    }
    characters[start as usize..end as usize].iter().collect()
}

/// An integer argument, tolerating the numeric strings a template's extracted
/// values are.
fn int_argument(value: &JinjaValue) -> Option<i64> {
    if let Ok(number) = i64::try_from(value.clone()) {
        return Some(number);
    }
    let text = value.to_string();
    text.parse::<i64>()
        .or_else(|_| text.parse::<f64>().map(|number| number.trunc() as i64))
        .ok()
}

fn parse_i64(value: &JinjaValue) -> Result<i64, Error> {
    value.to_string().parse().map_err(|_| {
        Error::new(
            ErrorKind::InvalidOperation,
            format!("cannot convert {value} to int"),
        )
    })
}

fn parse_f64(value: &JinjaValue) -> Result<f64, Error> {
    value.to_string().parse().map_err(|_| {
        Error::new(
            ErrorKind::InvalidOperation,
            format!("cannot convert {value} to float"),
        )
    })
}

fn fake_category(category: &str) -> Result<String, Error> {
    use fake::faker::address::en::*;
    use fake::faker::company::en::*;
    use fake::faker::internet::en::*;
    use fake::faker::name::en::*;
    use fake::faker::phone_number::en::*;

    let result: String = match category {
        "name" => Name().fake(),
        "first_name" => FirstName().fake(),
        "last_name" => LastName().fake(),
        "email" => SafeEmail().fake(),
        "username" => Username().fake(),
        "password" => Password(8..16).fake(),
        "ipv4" => IPv4().fake(),
        "ipv6" => IPv6().fake(),
        "user_agent" => UserAgent().fake(),
        "company" => CompanyName().fake(),
        "city" => CityName().fake(),
        "country" => CountryName().fake(),
        "phone" => PhoneNumber().fake(),
        other => {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                format!("unknown fake category: {other}"),
            ));
        }
    };
    Ok(result)
}

/// Raw bytes of a value: bytes values pass through, everything else renders to
/// text and is encoded as UTF-8 (Python str.encode('utf-8') equivalent).
fn value_bytes(value: &JinjaValue) -> Vec<u8> {
    if let Some(bytes) = value.as_bytes() {
        return bytes.to_vec();
    }
    value.to_string().into_bytes()
}

/// QD is_num: digit check on the string form, allowing a single decimal point.
fn qd_is_num(value: &JinjaValue) -> bool {
    let s = value.to_string();
    let is_digits = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
    if s.matches('.').count() == 1 {
        let (int_part, frac_part) = s.split_once('.').unwrap();
        is_digits(int_part.trim_start_matches('-')) && is_digits(frac_part)
    } else {
        is_digits(s.trim_start_matches('-'))
    }
}

/// QD to_bool: only 'yes'/'on'/'1'/'true' count as true.
fn qd_bool(value: &JinjaValue) -> bool {
    if value.is_none() || value.is_undefined() {
        return false;
    }
    if value.kind() == minijinja::value::ValueKind::Bool {
        return value.is_true();
    }
    let lowered = value.to_string().to_lowercase();
    matches!(lowered.as_str(), "yes" | "on" | "1" | "true")
}

#[derive(Clone, Copy)]
enum QdArith {
    Add,
    Sub,
    Mul,
    Div,
}

/// QD add/sub/multiply/divide: variadic float chain. Non-numeric first argument
/// yields int 0, a non-numeric (or zero for divide) later argument yields None.
fn qd_arith(values: &[JinjaValue], op: QdArith) -> JinjaValue {
    if values.is_empty() || !qd_is_num(&values[0]) {
        return JinjaValue::from(0i64);
    }
    let mut result = values[0].to_string().parse::<f64>().unwrap_or_default();
    for value in &values[1..] {
        if !qd_is_num(value) {
            return JinjaValue::from(());
        }
        let parsed = value.to_string().parse::<f64>().unwrap_or_default();
        match op {
            QdArith::Add => result += parsed,
            QdArith::Sub => result -= parsed,
            QdArith::Mul => result *= parsed,
            QdArith::Div => {
                if parsed == 0.0 {
                    return JinjaValue::from(());
                }
                result /= parsed;
            }
        }
    }
    JinjaValue::from(format!("{result:.6}"))
}

/// QD conver2unicode: net effect of utils.conver2unicode - decode \uXXXX and
/// \xNN escape sequences embedded in the text, leave plain text untouched.
pub(crate) fn conver2unicode(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(current) = chars.next() {
        if current != '\\' {
            result.push(current);
            continue;
        }
        match chars.peek().copied() {
            Some('u') => {
                chars.next();
                let hex: String = chars.by_ref().take(4).collect();
                if let Ok(code) = u32::from_str_radix(&hex, 16) {
                    if let Some(decoded) = char::from_u32(code) {
                        result.push(decoded);
                    } else {
                        result.push_str(&format!("\\u{hex}"));
                    }
                } else {
                    result.push_str(&format!("\\u{hex}"));
                }
            }
            Some('x') => {
                chars.next();
                let hex: String = chars.by_ref().take(2).collect();
                match u32::from_str_radix(&hex, 16) {
                    Ok(code) => {
                        // Python unicode_escape decodes \xNN as a latin-1 char.
                        let decoded = if code < 128 {
                            char::from_u32(code).unwrap_or('\u{fffd}')
                        } else {
                            // Decode the latin-1 codepoint as its UTF-8 form via lossy byte.
                            let byte = code as u8;
                            match std::str::from_utf8(&[byte]) {
                                Ok(text) => text.chars().next().unwrap_or('\u{fffd}'),
                                // Latin-1 supplement: map byte to the codepoint char.
                                Err(_) => char::from_u32(code).unwrap_or('\u{fffd}'),
                            }
                        };
                        result.push(decoded);
                    }
                    Err(_) => result.push_str(&format!("\\x{hex}")),
                }
            }
            Some('n') => {
                chars.next();
                result.push('\n');
            }
            Some('r') => {
                chars.next();
                result.push('\r');
            }
            Some('t') => {
                chars.next();
                result.push('\t');
            }
            Some('\\') => {
                chars.next();
                result.push('\\');
            }
            _ => result.push(current),
        }
    }
    result
}

/// binascii.b2a_hex with sep/bytes_per_sep: insert the separator between hex
/// groups of `bytes_per_sep` input bytes (2 hex chars each); positive counts
/// group from the right, negative from the left.
fn hex_with_sep(data: &[u8], sep: &str, bytes_per_sep: i64) -> String {
    let encoded = hex::encode(data);
    if bytes_per_sep == 0 || encoded.is_empty() {
        return encoded;
    }
    let group = bytes_per_sep.unsigned_abs() as usize * 2;
    if bytes_per_sep > 0 {
        let mut parts: Vec<&str> = Vec::new();
        let mut end = encoded.len();
        while end > 0 {
            let start = end.saturating_sub(group);
            parts.push(&encoded[start..end]);
            end = start;
        }
        parts.reverse();
        parts.join(sep)
    } else {
        encoded
            .as_bytes()
            .chunks(group)
            .map(|chunk| std::str::from_utf8(chunk).expect("hex is ascii"))
            .collect::<Vec<_>>()
            .join(sep)
    }
}

/// binascii.b2a_uu: one uuencoded line with the length header and trailing \n.
fn uuencode_line(data: &[u8]) -> String {
    if data.is_empty() {
        return "`\n".to_string();
    }
    let mut out = String::new();
    out.push((b' ' + data.len() as u8) as char);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push((b' ' + (b0 >> 2)) as char);
        out.push((b' ' + (((b0 & 0x03) << 4) | (b1 >> 4))) as char);
        out.push((b' ' + (((b1 & 0x0f) << 2) | (b2 >> 6))) as char);
        out.push((b' ' + (b2 & 0x3f)) as char);
    }
    out.push('\n');
    out
}

/// binascii.a2b_uu: decode a single uuencoded line.
fn uudecode_line(line: &str) -> Result<Vec<u8>, Error> {
    let Some(first) = line.chars().next() else {
        return Ok(Vec::new());
    };
    let length = first as u32 - b' ' as u32;
    let bytes: Vec<u8> = line
        .chars()
        .skip(1)
        .filter(|c| c.is_ascii())
        .map(|c| (c as u8).saturating_sub(b' ').min(63))
        .collect();
    let mut decoded = Vec::with_capacity(bytes.len() * 3 / 4 + 3);
    for group in bytes.chunks(4) {
        let mut group = group.to_vec();
        while group.len() < 4 {
            group.push(0);
        }
        decoded.push((group[0] << 2) | (group[1] >> 4));
        decoded.push(((group[1] & 0x0f) << 4) | (group[2] >> 2));
        decoded.push(((group[2] & 0x03) << 6) | group[3]);
    }
    decoded.truncate(length as usize);
    Ok(decoded)
}

/// binascii.b2a_qp: quoted-printable encoding with 76-char soft line breaks.
fn qp_encode(data: &[u8], quotetabs: bool, istext: bool) -> String {
    let mut out = String::new();
    let mut line_len = 0usize;
    let mut pending: Vec<String> = Vec::new();
    let flush_pending = |out: &mut String, pending: &mut Vec<String>, line_len: &mut usize| {
        // Trailing space/tab on a line must be encoded.
        if pending.len() == 1 && (pending[0] == " " || pending[0] == "\t") {
            let encoded: &str = if pending[0] == " " { "=20" } else { "=09" };
            *line_len += 3;
            out.push_str(encoded);
        } else {
            for token in pending.iter() {
                *line_len += token.len();
                out.push_str(token);
            }
        }
        pending.clear();
    };
    for &byte in data {
        let token = match byte {
            b'=' => "=3D".to_string(),
            b'\r' if istext => "\r".to_string(),
            b'\n' if istext => {
                flush_pending(&mut out, &mut pending, &mut line_len);
                line_len = 0;
                out.push('\n');
                continue;
            }
            b' ' | b'\t' if !quotetabs => {
                pending.push((byte as char).to_string());
                continue;
            }
            0x21..=0x7e => (byte as char).to_string(),
            _ => format!("={byte:02X}"),
        };
        if line_len + token.len() > 75 {
            flush_pending(&mut out, &mut pending, &mut line_len);
            out.push_str("=\r\n");
            line_len = 0;
        }
        line_len += token.len();
        out.push_str(&token);
    }
    flush_pending(&mut out, &mut pending, &mut line_len);
    out
}

/// binascii.a2b_qp: quoted-printable decoding; invalid escapes are kept.
fn qp_decode(text: &str) -> Result<Vec<u8>, Error> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'=' {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 1 < bytes.len() && (bytes[index + 1] == b'\n' || bytes[index + 1] == b'\r') {
            // Soft line break; skip the whole CRLF/LF sequence.
            index += if bytes[index + 1] == b'\r'
                && index + 2 < bytes.len()
                && bytes[index + 2] == b'\n'
            {
                3
            } else {
                2
            };
            continue;
        }
        if index + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("zz"),
                16,
            )
        {
            out.push(byte);
            index += 3;
            continue;
        }
        out.push(b'=');
        index += 1;
    }
    Ok(out)
}

/// zlib.crc32-compatible IEEE CRC-32.
fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (index, entry) in table.iter_mut().enumerate() {
        let mut crc = index as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
        *entry = crc;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = table[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// binascii.crc_hqx: CRC-CCITT (XModem), polynomial 0x1021.
fn crc_hqx(data: &[u8], initial: u16) -> u16 {
    let mut crc = initial;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Python re.sub replacement syntax (\1, \g<name>) mapped onto Rust regex
/// syntax ($1, ${name}); literal dollars are escaped first.
fn python_replacement(replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len());
    let mut chars = replacement.chars().peekable();
    while let Some(current) = chars.next() {
        if current != '\\' {
            if current == '$' {
                out.push_str("$$");
            } else {
                out.push(current);
            }
            continue;
        }
        match chars.peek().copied() {
            Some('g') => {
                chars.next();
                if chars.peek() == Some(&'<') {
                    chars.next();
                    let mut name = String::new();
                    for ch in chars.by_ref() {
                        if ch == '>' {
                            break;
                        }
                        name.push(ch);
                    }
                    out.push_str(&format!("${{{name}}}"));
                } else {
                    out.push_str("\\g");
                }
            }
            Some(digit) if digit.is_ascii_digit() => {
                chars.next();
                out.push('$');
                out.push(digit);
            }
            Some('\\') => {
                chars.next();
                out.push_str("$$\\");
            }
            _ => out.push('\\'),
        }
    }
    out
}

fn qd_regex(pattern: &str, ignorecase: bool, multiline: bool) -> Result<Regex, Error> {
    let flags = match (ignorecase, multiline) {
        (true, true) => "(?im)",
        (true, false) => "(?i)",
        (false, true) => "(?m)",
        (false, false) => "",
    };
    Regex::new(&format!("{flags}{pattern}"))
        .map_err(|e| Error::new(ErrorKind::InvalidOperation, format!("invalid regex: {e}")))
}

/// Python str(list) representation: ['a', 'b'].
fn py_list_repr(items: &[String]) -> String {
    let rendered: Vec<String> = items.iter().map(|item| format!("'{item}'")).collect();
    format!("[{}]", rendered.join(", "))
}

/// Python str(tuple) representation: ('a', 'b').
fn py_tuple_repr(items: &[String]) -> String {
    let rendered: Vec<String> = items.iter().map(|item| format!("'{item}'")).collect();
    if rendered.len() == 1 {
        format!("({},)", rendered[0])
    } else {
        format!("({})", rendered.join(", "))
    }
}

/// Python builtin format(value, spec) for the specs seen in QD templates:
/// precision floats (.2f), integer bases (d, x, X, o, b), strings and width
/// padding. Unsupported specs fall back to the string form.
fn python_format(value: &JinjaValue, spec: &str) -> Result<String, Error> {
    if spec.is_empty() {
        return Ok(value.to_string());
    }
    let mut rest = spec;
    let (fill, align) = if rest.len() >= 2 && matches!(rest.as_bytes()[1], b'<' | b'>' | b'^') {
        let fill = rest.chars().next().unwrap();
        let align = rest.as_bytes()[1] as char;
        rest = &rest[2..];
        (Some(fill), Some(align))
    } else if let Some(first) = rest.chars().next() {
        if matches!(first, '<' | '>' | '^') {
            rest = &rest[first.len_utf8()..];
            (None, Some(first))
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };
    if rest.starts_with(['+', '-', ' ']) {
        rest = &rest[1..];
    }
    let zero_pad = rest.starts_with('0');
    if zero_pad {
        rest = &rest[1..];
    }
    let width_part_len = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let width: Option<usize> = if width_part_len > 0 {
        Some(
            rest[..width_part_len]
                .parse()
                .map_err(|_| Error::new(ErrorKind::InvalidOperation, "invalid format width"))?,
        )
    } else {
        None
    };
    rest = &rest[width_part_len..];
    let precision = if let Some(stripped) = rest.strip_prefix('.') {
        let digits_len = stripped
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(stripped.len());
        let parsed: usize = stripped[..digits_len]
            .parse()
            .map_err(|_| Error::new(ErrorKind::InvalidOperation, "invalid format precision"))?;
        rest = &stripped[digits_len..];
        Some(parsed)
    } else {
        None
    };
    let spec_type = rest.chars().next();
    let precision_value = precision.unwrap_or(6);

    let body =
        match spec_type {
            None | Some('s') => {
                if value.is_number() && precision.is_some() {
                    let number = value.to_string().parse::<f64>().unwrap_or_default();
                    format!("{number:.precision_value$}")
                } else {
                    value.to_string()
                }
            }
            Some('f') => {
                let number = value.to_string().parse::<f64>().map_err(|_| {
                    Error::new(ErrorKind::InvalidOperation, "invalid float format value")
                })?;
                format!("{number:.precision_value$}")
            }
            Some('d') => {
                let number = value.to_string().parse::<i64>().map_err(|_| {
                    Error::new(ErrorKind::InvalidOperation, "invalid int format value")
                })?;
                number.to_string()
            }
            Some('x') | Some('X') | Some('o') | Some('b') => {
                let number = value.to_string().parse::<i64>().map_err(|_| {
                    Error::new(ErrorKind::InvalidOperation, "invalid int format value")
                })?;
                match spec_type {
                    Some('x') => format!("{number:x}"),
                    Some('X') => format!("{number:X}"),
                    Some('o') => format!("{number:o}"),
                    _ => format!("{number:b}"),
                }
            }
            Some('e') => {
                let number = value.to_string().parse::<f64>().map_err(|_| {
                    Error::new(ErrorKind::InvalidOperation, "invalid float format value")
                })?;
                format!("{number:e}")
            }
            Some(other) => {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    format!("unsupported format spec: {other}"),
                ));
            }
        };

    let padded = match (width, align) {
        (Some(width), _) if body.len() < width => {
            let padding = width - body.len();
            match align {
                Some('<') => format!("{body}{}", " ".repeat(padding)),
                Some('^') => {
                    let left = padding / 2;
                    let right = padding - left;
                    format!("{}{body}{}", " ".repeat(left), " ".repeat(right))
                }
                _ => {
                    let pad_char = if zero_pad && align.is_none() {
                        "0"
                    } else {
                        " "
                    };
                    if pad_char == "0" && body.starts_with('-') {
                        format!("-{}{}", "0".repeat(padding.saturating_sub(1)), &body[1..])
                    } else {
                        format!("{}{body}", pad_char.repeat(padding))
                    }
                }
            }
        }
        _ => body,
    };
    let _ = fill;
    Ok(padded)
}

/// Apply AES (CBC or ECB) with pkcs7 padding to `data`.
fn aes_apply(
    key: &str,
    mode: &str,
    iv: Option<&str>,
    data: &[u8],
    padding: bool,
    padding_style: &str,
    encrypt: bool,
) -> Result<Vec<u8>, Error> {
    use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};

    if !padding {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            "aes without pkcs7 padding is not supported by qdrust",
        ));
    }
    if !padding_style.eq_ignore_ascii_case("pkcs7") {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            format!("unsupported aes padding style: {padding_style}"),
        ));
    }
    let upper = mode.to_uppercase();
    match upper.as_str() {
        "CBC" => {
            let iv = iv.ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidOperation,
                    "aes CBC requires an iv (QD generates a random one)",
                )
            })?;
            if iv.len() != 16 {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    format!("invalid aes iv length: {}", iv.len()),
                ));
            }
            type Enc128 = cbc::Encryptor<aes::Aes128>;
            type Enc192 = cbc::Encryptor<aes::Aes192>;
            type Enc256 = cbc::Encryptor<aes::Aes256>;
            type Dec128 = cbc::Decryptor<aes::Aes128>;
            type Dec192 = cbc::Decryptor<aes::Aes192>;
            type Dec256 = cbc::Decryptor<aes::Aes256>;
            if encrypt {
                let ciphertext = match key.len() {
                    16 => Enc128::new(key.as_bytes().into(), iv.as_bytes().into())
                        .encrypt_padded_vec_mut::<Pkcs7>(data),
                    24 => Enc192::new(key.as_bytes().into(), iv.as_bytes().into())
                        .encrypt_padded_vec_mut::<Pkcs7>(data),
                    32 => Enc256::new(key.as_bytes().into(), iv.as_bytes().into())
                        .encrypt_padded_vec_mut::<Pkcs7>(data),
                    other => {
                        return Err(Error::new(
                            ErrorKind::InvalidOperation,
                            format!("invalid aes key length: {other}"),
                        ));
                    }
                };
                Ok(ciphertext)
            } else {
                let plaintext = match key.len() {
                    16 => Dec128::new(key.as_bytes().into(), iv.as_bytes().into())
                        .decrypt_padded_vec_mut::<Pkcs7>(data)
                        .map_err(aes_error)?,
                    24 => Dec192::new(key.as_bytes().into(), iv.as_bytes().into())
                        .decrypt_padded_vec_mut::<Pkcs7>(data)
                        .map_err(aes_error)?,
                    32 => Dec256::new(key.as_bytes().into(), iv.as_bytes().into())
                        .decrypt_padded_vec_mut::<Pkcs7>(data)
                        .map_err(aes_error)?,
                    other => {
                        return Err(Error::new(
                            ErrorKind::InvalidOperation,
                            format!("invalid aes key length: {other}"),
                        ));
                    }
                };
                Ok(plaintext)
            }
        }
        "ECB" => {
            use ecb::cipher::{
                BlockDecryptMut, BlockEncryptMut, KeyInit, block_padding::Pkcs7 as EcbPkcs7,
            };

            type Enc128 = ecb::Encryptor<aes::Aes128>;
            type Enc192 = ecb::Encryptor<aes::Aes192>;
            type Enc256 = ecb::Encryptor<aes::Aes256>;
            type Dec128 = ecb::Decryptor<aes::Aes128>;
            type Dec192 = ecb::Decryptor<aes::Aes192>;
            type Dec256 = ecb::Decryptor<aes::Aes256>;
            if encrypt {
                let ciphertext =
                    match key.len() {
                        16 => Enc128::new(key.as_bytes().into())
                            .encrypt_padded_vec_mut::<EcbPkcs7>(data),
                        24 => Enc192::new(key.as_bytes().into())
                            .encrypt_padded_vec_mut::<EcbPkcs7>(data),
                        32 => Enc256::new(key.as_bytes().into())
                            .encrypt_padded_vec_mut::<EcbPkcs7>(data),
                        other => {
                            return Err(Error::new(
                                ErrorKind::InvalidOperation,
                                format!("invalid aes key length: {other}"),
                            ));
                        }
                    };
                Ok(ciphertext)
            } else {
                let plaintext = match key.len() {
                    16 => Dec128::new(key.as_bytes().into())
                        .decrypt_padded_vec_mut::<EcbPkcs7>(data)
                        .map_err(aes_error)?,
                    24 => Dec192::new(key.as_bytes().into())
                        .decrypt_padded_vec_mut::<EcbPkcs7>(data)
                        .map_err(aes_error)?,
                    32 => Dec256::new(key.as_bytes().into())
                        .decrypt_padded_vec_mut::<EcbPkcs7>(data)
                        .map_err(aes_error)?,
                    other => {
                        return Err(Error::new(
                            ErrorKind::InvalidOperation,
                            format!("invalid aes key length: {other}"),
                        ));
                    }
                };
                Ok(plaintext)
            }
        }
        other => Err(Error::new(
            ErrorKind::InvalidOperation,
            format!("unsupported aes mode: {other} (qdrust supports CBC and ECB)"),
        )),
    }
}

fn aes_error(cause: impl std::fmt::Display) -> Error {
    Error::new(
        ErrorKind::InvalidOperation,
        format!("aes decrypt failed: {cause}"),
    )
}

/// QD/mcrypto output formatting: base64 in Python's encodebytes style (76-char
/// lines with a trailing newline) or plain hex.
fn aes_format_output(data: &[u8], output_format: &str) -> String {
    match output_format.to_lowercase().as_str() {
        "hex" => hex::encode(data),
        _ => {
            let encoded = BASE64.encode(data);
            let mut wrapped = String::new();
            let bytes = encoded.as_bytes();
            let mut start = 0usize;
            while start < bytes.len() {
                let end = (start + 76).min(bytes.len());
                wrapped.push_str(std::str::from_utf8(&bytes[start..end]).expect("base64 is ascii"));
                wrapped.push('\n');
                start = end;
            }
            if wrapped.is_empty() {
                wrapped.push('\n');
            }
            wrapped
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The accumulator idiom from the issue-#27 pcbeta template, verbatim in
    /// shape: a list declared in the template, built with `.append` inside a
    /// `{% for %}`, then joined. Under plain MiniJinja this dies with
    /// "sequence has no method named append", and no `{% set %}` rewrite can
    /// save it because set does not persist across iterations.
    #[test]
    fn an_append_accumulator_survives_the_for_loop() {
        let engine = QdExpressionEngine::default();
        let mut variables = BTreeMap::new();
        variables.insert("cn".to_string(), json!(["a b", "c"]));

        let template = "{% set parts = [] %}\n\
            {% for i in range(cn|length) %}\n\
            {% set n = cn[i] | urlencode %}\n\
            {% if n %}\n\
            {% set _ = parts.append(n ~ \"=\" ~ i) %}\n\
            {% endif %}\n\
            {% endfor %}\n\
            {{ parts | join(\"; \") }}";
        let rendered = engine.render(template, &variables).unwrap();
        // Text between the tags is preserved as newlines; only the joined
        // accumulator matters.
        assert_eq!(rendered.trim(), "a%20b=0; c=1");
    }

    #[test]
    fn the_other_python_list_mutators_work_too() {
        let engine = QdExpressionEngine::default();
        let variables = BTreeMap::new();

        let rendered = engine
            .render(
                "{% set parts = ['a'] %}{% set _ = parts.extend(['b', 'c']) %}\
                 {% set _ = parts.insert(0, 'z') %}{% set _ = parts.remove('b') %}\
                 {% set popped = parts.pop() %}{{ parts|join(',') }}|{{ popped }}",
                &variables,
            )
            .unwrap();
        assert_eq!(rendered, "z,a|c");

        // A negative index counts from the end, like Python's.
        let rendered = engine
            .render(
                "{% set parts = ['x', 'y'] %}{{ parts.pop(-1) }}|{{ parts|length }}",
                &variables,
            )
            .unwrap();
        assert_eq!(rendered, "y|1");
    }

    #[test]
    fn a_mutable_list_behaves_like_a_plain_array_everywhere_else() {
        let engine = QdExpressionEngine::default();
        let variables = BTreeMap::new();

        // Iteration, length, indexing, comparisons, re-declaration as a reset,
        // and the JSON an extracted variable would see.
        let rendered = engine
            .render(
                "{% set parts = [] %}{% set _ = parts.append(2) %}{% set _ = parts.append(1) %}\
                 {{ parts }}|{{ parts|length }}|{{ parts[1] }}|{{ parts == [2, 1] }}|\
                 {% set parts = [] %}{{ parts|length }}",
                &variables,
            )
            .unwrap();
        assert_eq!(rendered, "[2, 1]|2|1|True|0");
    }

    #[test]
    fn a_list_that_came_from_a_variable_still_cannot_be_mutated() {
        let engine = QdExpressionEngine::default();
        let mut variables = BTreeMap::new();
        variables.insert("cn".to_string(), json!(["a"]));

        // Values passed in from outside the template are shared and immutable;
        // the rewrite only touches lists the template declares itself.
        let error = engine
            .render("{% set _ = cn.append('b') %}", &variables)
            .unwrap_err();
        assert!(format!("{error:#}").contains("has no method named append"));
    }

    #[test]
    fn the_rewrite_leaves_templates_without_mutators_alone() {
        let source = "{% set parts = [] %}{{ parts|length }}";
        assert!(matches!(
            compat_python_template(source),
            std::borrow::Cow::Borrowed(_)
        ));

        let source = "{% set parts = [] %}{% set _ = parts.append(1) %}";
        let rewritten = compat_python_template(source);
        assert!(rewritten.contains("__qd_list([])"));
        // Whitespace-control dashes survive, and a seeded literal is carried
        // into the mutable object.
        let rewritten =
            compat_python_template("{%- set parts = ['a'] -%}{% set _ = parts.append(1) %}");
        assert!(rewritten.contains("{%- set parts = __qd_list(['a']) -%}"));
    }

    #[test]
    fn the_mutable_list_global_is_not_a_required_variable() {
        // QD subtracts engine-known names from a template's required variables;
        // the internal accumulator helper must be on that list.
        assert!(
            QdExpressionEngine::default()
                .known_names()
                .contains("__qd_list")
        );
    }

    /// QD templates call Python's string methods on extracted text, and this
    /// engine has to answer them: a bare MiniJinja value has no methods at all
    /// ("unknown method"), and extraction only ever stores strings and JSON, so
    /// `s.strip()` is how a template cleans a scraped value up.
    #[test]
    fn python_string_methods_read_extracted_text() {
        let engine = QdExpressionEngine::default();
        let mut variables = BTreeMap::new();
        variables.insert("s".to_string(), json!("  Hello, World  "));
        variables.insert("csv".to_string(), json!("a,b,,c"));

        let rendered = engine
            .render(
                "{{ s.strip() }}|{{ s.strip().lower() }}|{{ s.strip().replace('l', 'L') }}|\
                 {{ csv.split(',')|join(';') }}|{{ csv.split(',')[0] }}|\
                 {{ csv.split(',')|length }}",
                &variables,
            )
            .unwrap();
        assert_eq!(
            rendered,
            "Hello, World|hello, world|HeLLo, WorLd|a;b;;c|a|4"
        );

        // A literal receiver is rewritten the same way, and the predicates and
        // padding take their Python meanings.
        let rendered = engine
            .render(
                "{{ 'abc'.startswith('ab') }}|{{ 'abc'.endswith(('x', 'c')) }}|\
                 {{ 'abcabc'.find('c') }}|{{ 'abcabc'.rfind('c') }}|\
                 {{ '7'.zfill(3) }}|{{ 'ab'.center(6, '-') }}|\
                 {{ '{}-{}'.format('a', 'b') }}",
                &BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(rendered, "True|True|2|5|007|--ab--|a-b");
    }

    /// A `dict` receiver answers `get`/`keys`/`values`/`items`, which is how a
    /// JSON extraction is walked.
    #[test]
    fn python_dict_methods_read_extracted_objects() {
        let engine = QdExpressionEngine::default();
        let mut variables = BTreeMap::new();
        variables.insert("o".to_string(), json!({"a": 1, "b": 2}));

        let rendered = engine
            .render(
                "{{ o.get('a') }}|{{ o.get('z', 'dflt') }}|{{ o.keys()|join(',') }}|\
                 {{ o.values()|join(',') }}|{{ o.items()|length }}",
                &variables,
            )
            .unwrap();
        assert_eq!(rendered, "1|dflt|a,b|1,2|2");

        // The documented loop form, which unpacks each pair the method returns.
        let rendered = engine
            .render(
                "{% for k, v in o.items() %}{{ k }}={{ v }};{% endfor %}",
                &variables,
            )
            .unwrap();
        assert_eq!(rendered, "a=1;b=2;");
    }

    /// A name that is not a method stays a call to nothing — the rewrite must
    /// not invent one and must not silently swallow the error.
    #[test]
    fn an_unknown_method_is_reported_rather_than_guessed() {
        let engine = QdExpressionEngine::default();
        let mut variables = BTreeMap::new();
        variables.insert("s".to_string(), json!("x"));

        let error = engine.render("{{ s.nope() }}", &variables).unwrap_err();
        assert!(format!("{error:#}").contains("nope"), "{error:#}");
    }

    /// The Jinja2 filters QD templates lean on that a bare MiniJinja does not
    /// carry. `striptags` is the pcbeta template's, the rest come with it.
    #[test]
    fn the_jinja2_builtin_filters_qd_templates_use() {
        let engine = QdExpressionEngine::default();
        let variables = BTreeMap::new();

        let rendered = engine
            .render("{{ '<p>Hello   <b>World</b></p>'|striptags }}", &variables)
            .unwrap();
        assert_eq!(rendered, "Hello World");

        let rendered = engine
            .render("{{ 'Hello big world'|wordcount }}", &variables)
            .unwrap();
        assert_eq!(rendered, "3");

        // `truncate` counts the ellipsis in `length` and, without `killwords`,
        // backs up to the last whole word; `leeway` is pinned so the boundary
        // is the one under test.
        let rendered = engine
            .render(
                "{{ 'foo bar baz qux'|truncate(9, true, leeway=0) }}|\
                 {{ 'foo bar baz qux'|truncate(9, false, leeway=0) }}",
                &variables,
            )
            .unwrap();
        assert_eq!(rendered, "foo ba...|foo...");

        let rendered = engine
            .render("{{ 'aaa bbb ccc'|wordwrap(7) }}", &variables)
            .unwrap();
        assert_eq!(rendered, "aaa bbb\nccc");

        let rendered = engine.render("{{ 'ab'|center(6) }}", &variables).unwrap();
        assert_eq!(rendered, "  ab  ");

        let rendered = engine
            .render(
                "{{ 1|filesizeformat }}|{{ 1000|filesizeformat }}|\
                 {{ 1024|filesizeformat(true) }}",
                &variables,
            )
            .unwrap();
        assert_eq!(rendered, "1 Byte|1.0 kB|1.0 KiB");

        // `xmlattr` drops a null entry and escapes the rest, with the leading
        // space Jinja2 writes.
        let rendered = engine
            .render(
                r#"{{ {'class': 'btn"x', 'id': none}|xmlattr }}"#,
                &variables,
            )
            .unwrap();
        assert_eq!(rendered, r#" class="btn&#34;x""#);
    }

    /// `"%s" % x`, which QD templates use to build URLs and messages. The left
    /// operand is a string literal, so the rewrite knows what it is reading.
    #[test]
    fn the_percent_operator_formats_like_python() {
        let engine = QdExpressionEngine::default();
        let mut variables = BTreeMap::new();
        variables.insert("name".to_string(), json!("World"));
        variables.insert("fields".to_string(), json!({"n": "Bob", "c": 3}));
        variables.insert("args".to_string(), json!(["a", "b"]));

        let rendered = engine
            .render(
                "{{ 'Hello, %s!' % name }}|{{ '%d items' % 42 }}|\
                 {{ '%05.2f' % 3.14159 }}|{{ '%x' % 255 }}|{{ '%s%%' % 5 }}|\
                 {{ '%(n)s has %(c)d' % fields }}|{{ '%s-%s' % args }}|\
                 {{ '%s-%s' % ('a', 'b') }}",
                &variables,
            )
            .unwrap();
        assert_eq!(
            rendered,
            "Hello, World!|42 items|03.14|ff|5%|Bob has 3|a-b|a-b"
        );
    }

    #[test]
    fn qd_globals_are_also_registered_as_filters() {
        let engine = QdExpressionEngine::default();
        let variables = BTreeMap::new();

        // QD registers every jinja global as a filter as well
        // (fetcher.py: jinja_env.filters.update(jinja_globals)).
        let rendered = engine
            .render("{{ 'a b'|urlencode }}", &variables)
            .expect("urlencode filter must exist");
        assert_eq!(rendered, "a%20b");

        let rendered = engine
            .render("{{ 'hello'|md5 }}", &variables)
            .expect("md5 filter must exist");
        assert_eq!(rendered, "5d41402abc4b2a76b9719d911017c592");

        let rendered = engine
            .render("{{ 'aGVsbG8='|b64decode }}", &variables)
            .expect("b64decode filter must exist");
        assert_eq!(rendered, "hello");

        // Chains lifted from the 189天翼云 template, through the render path.
        let variables = BTreeMap::from([
            ("passrsakey".to_string(), serde_json::json!("aGVsbG8=")),
            ("username".to_string(), serde_json::json!("user name")),
        ]);
        let rendered = engine
            .render(
                "{{ unicode(b2a_hex(a2b_base64(passrsakey), sep=' ', bytes_per_sep=1))|urlencode }}",
                &variables,
            )
            .expect("chained binascii render must work");
        assert_eq!(rendered, "68%2065%206c%206c%206f");

        let rendered = engine
            .render("{{ username|urlencode }}", &variables)
            .expect("urlencode variable render must work");
        assert_eq!(rendered, "user%20name");

        let rendered = engine
            .render(
                "{{ multiply(timestamp('float'),1000)|urlencode }}",
                &variables,
            )
            .expect("multiply chain render must work");
        assert!(
            Regex::new(r"^1\d{12}\.\d{6}$").unwrap().is_match(&rendered),
            "unexpected multiply output: {rendered}"
        );
    }

    #[test]
    fn finds_undeclared_variables_behind_filters_and_functions() {
        let engine = QdExpressionEngine::default();

        // QD filters/functions are not inputs, even when they wrap one.
        assert_eq!(
            engine.undeclared_variables("username={{jpop_username|urlencode}}"),
            ["jpop_username"]
        );
        assert_eq!(
            engine.undeclared_variables("{{ md5(password) }}"),
            ["password"]
        );
        // Attribute access reports the root name, like jinja2.meta.
        assert_eq!(engine.undeclared_variables("{{ user.name }}"), ["user"]);
        // Literals contribute nothing.
        assert!(engine.undeclared_variables("{{ 'username' }}").is_empty());
        // Source order is preserved so the form reads like the template.
        assert_eq!(
            engine.undeclared_variables("{{ beta }} {{ alpha }} {{ beta }}"),
            ["beta", "alpha"]
        );
    }

    #[test]
    fn invalid_jinja_control_fragments_yield_no_variables() {
        // QD parses every field on its own with a bare `Environment()`; a
        // `while` tag or a dangling `endif` is a syntax error there and must
        // not invent variables here either.
        let engine = QdExpressionEngine::default();
        assert!(
            engine
                .undeclared_variables("{% while int(loop_index0) < 100 and task != 'no' %}")
                .is_empty()
        );
        assert!(engine.undeclared_variables("{% endwhile %}").is_empty());
        // Valid control flow still contributes its inputs.
        assert_eq!(
            engine.undeclared_variables("{% if token %}ok{% endif %}"),
            ["token"]
        );
    }

    #[test]
    fn totp_matches_the_rfc_6238_vectors() {
        let engine = QdExpressionEngine::default();
        let variables = BTreeMap::new();
        let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
        // Function form, timestamp pinned so the vector is exact.
        let rendered = engine
            .render(
                &format!("{{{{ totp(\"{secret}\", 8, 30, \"sha1\", 59) }}}}"),
                &variables,
            )
            .expect("totp function must exist");
        assert_eq!(rendered, "94287082");
        // Defaults are 6 digits / 30s / sha1, and the filter form works too.
        let rendered = engine
            .render(
                &format!("{{{{ \"{secret}\"|totp(6, 30, \"sha1\", 59) }}}}"),
                &variables,
            )
            .expect("totp filter must exist");
        assert_eq!(rendered, "287082");
        // A secret that is not base32 fails the render instead of silently
        // producing a wrong code.
        assert!(
            engine
                .render("{{ totp('not base32!') }}", &variables)
                .is_err()
        );
    }

    #[test]
    fn evaluates_qd_boolean_and_conversion_expression() {
        let engine = QdExpressionEngine::default();
        let variables = BTreeMap::from([
            ("loop_index0".into(), json!("2")),
            ("While_Limit".into(), json!(3)),
            ("enabled".into(), json!(true)),
        ]);
        assert!(
            engine
                .evaluate_bool("int(loop_index0) < While_Limit and enabled", &variables)
                .unwrap()
        );
    }

    #[test]
    fn evaluates_qd_range_expression() {
        let engine = QdExpressionEngine::default();
        let value = engine.evaluate("range(1, 4)", &BTreeMap::new()).unwrap();
        assert_eq!(value, json!([1, 2, 3]));
    }

    #[test]
    fn evaluates_list_index_membership_and_length() {
        let engine = QdExpressionEngine::default();
        let variables = BTreeMap::from([("items".into(), json!(["a", "b"]))]);
        assert_eq!(
            engine.evaluate("list(items)", &variables).unwrap(),
            json!(["a", "b"])
        );
        assert!(
            engine
                .evaluate_bool(
                    "items[1] == 'b' and 'a' in items and len(items) == 2",
                    &variables
                )
                .unwrap()
        );
    }

    #[test]
    fn treats_missing_variable_condition_as_false() {
        let engine = QdExpressionEngine::default();
        assert!(
            !engine
                .evaluate_bool("missing_name", &BTreeMap::new())
                .unwrap()
        );
    }

    #[test]
    fn rejects_unsafe_python_syntax() {
        let engine = QdExpressionEngine::default();
        assert!(
            engine
                .evaluate("__import__('os').system('whoami')", &BTreeMap::new())
                .is_err()
        );
    }

    #[test]
    fn test_encoding_functions() {
        let engine = QdExpressionEngine::default();
        let vars = BTreeMap::new();

        // base64
        assert_eq!(
            engine.evaluate("b64encode('hello')", &vars).unwrap(),
            json!("aGVsbG8=")
        );
        assert_eq!(
            engine.evaluate("b64decode('aGVsbG8=')", &vars).unwrap(),
            json!("hello")
        );

        // hex (a2b_hex returns raw bytes; hex them back to compare, mirroring
        // the QD template chain b2a_hex(a2b_base64(x)))
        assert_eq!(
            engine.evaluate("b2a_hex(a2b_hex('6869'))", &vars).unwrap(),
            json!("6869")
        );

        // binascii chain from the 189天翼云 template: base64 -> bytes -> grouped hex
        assert_eq!(
            engine
                .evaluate(
                    "unicode(b2a_hex(a2b_base64('aGVsbG8='), sep=' ', bytes_per_sep=1))",
                    &vars
                )
                .unwrap(),
            json!("68 65 6c 6c 6f")
        );
        assert_eq!(
            engine
                .evaluate("b64encode(a2b_base64('aGVsbG8='))", &vars)
                .unwrap(),
            json!("aGVsbG8=")
        );

        // urlencode
        assert_eq!(
            engine.evaluate("urlencode('hello world')", &vars).unwrap(),
            json!("hello%20world")
        );
        // QD urlencode keeps "/" unescaped (urllib quote with safe="/")
        assert_eq!(
            engine.evaluate("urlencode('a/b c')", &vars).unwrap(),
            json!("a/b%20c")
        );

        // quote_chinese
        assert_eq!(
            engine.evaluate("quote_chinese('测试')", &vars).unwrap(),
            json!("%E6%B5%8B%E8%AF%95")
        );

        // url_decode and url_encode aliases
        assert_eq!(
            engine
                .evaluate("url_decode('hello%20world')", &vars)
                .unwrap(),
            json!("hello world")
        );
        assert_eq!(
            engine.evaluate("url_encode('hello world')", &vars).unwrap(),
            json!("hello%20world")
        );
    }

    #[test]
    fn test_hash_functions() {
        let engine = QdExpressionEngine::default();
        let vars = BTreeMap::new();

        // md5
        assert_eq!(
            engine.evaluate("md5('hello')", &vars).unwrap(),
            json!("5d41402abc4b2a76b9719d911017c592")
        );

        // sha1
        assert_eq!(
            engine.evaluate("sha1('hello')", &vars).unwrap(),
            json!("aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d")
        );

        // hash with different types
        assert_eq!(
            engine.evaluate("hash('hello', 'md5')", &vars).unwrap(),
            json!("5d41402abc4b2a76b9719d911017c592")
        );
        assert_eq!(
            engine.evaluate("hash('hello', 'sha256')", &vars).unwrap(),
            json!("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );

        // default to sha1
        assert_eq!(
            engine.evaluate("hash('hello')", &vars).unwrap(),
            json!("aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d")
        );
    }

    #[test]
    fn test_uuid_function() {
        let engine = QdExpressionEngine::default();
        let vars = BTreeMap::new();

        // UUID with default namespace (URL namespace)
        let result = engine.evaluate("to_uuid('example.com')", &vars).unwrap();
        assert!(result.is_string());
        assert_eq!(result.as_str().unwrap().len(), 36);

        // UUID with custom namespace
        let result = engine
            .evaluate(
                "to_uuid('test', '6ba7b810-9dad-11d1-80b4-00c04fd430c8')",
                &vars,
            )
            .unwrap();
        assert!(result.is_string());
        assert_eq!(result.as_str().unwrap().len(), 36);
    }

    #[test]
    fn test_time_functions() {
        let engine = QdExpressionEngine::default();
        let vars = BTreeMap::new();

        // timestamp as int
        let result = engine.evaluate("timestamp()", &vars).unwrap();
        assert!(result.is_number());
        assert!(result.as_u64().unwrap() > 1600000000);

        // timestamp as float
        let result = engine.evaluate("timestamp('float')", &vars).unwrap();
        assert!(result.is_number());
        assert!(result.as_f64().unwrap() > 1600000000.0);

        // date_time with default (both date and time)
        let result = engine.evaluate("date_time()", &vars).unwrap();
        let s = result.as_str().unwrap();
        assert!(s.contains('-'));
        assert!(s.contains(':'));

        // date_time with date only
        let result = engine.evaluate("date_time(true, false)", &vars).unwrap();
        let s = result.as_str().unwrap();
        assert!(s.contains('-'));
        assert!(!s.contains(':'));

        // date_time with time only
        let result = engine.evaluate("date_time(false, true)", &vars).unwrap();
        let s = result.as_str().unwrap();
        assert!(!s.contains('-'));
        assert!(s.contains(':'));

        // strftime without timestamp (current time)
        let result = engine.evaluate("strftime('%Y-%m-%d')", &vars).unwrap();
        let s = result.as_str().unwrap();
        assert_eq!(s.len(), 10);
        assert!(s.contains('-'));

        // strftime with specific timestamp
        let result = engine
            .evaluate("strftime('%Y-%m-%d', 1609459200)", &vars)
            .unwrap();
        assert_eq!(result.as_str().unwrap(), "2021-01-01");
    }

    #[test]
    fn test_math_operations() {
        let engine = QdExpressionEngine::default();
        let vars = BTreeMap::new();

        // add (QD returns f"{value:f}" strings)
        assert_eq!(
            engine.evaluate("add(5, 3)", &vars).unwrap(),
            json!("8.000000")
        );
        assert_eq!(
            engine.evaluate("add('5.5', '2.5')", &vars).unwrap(),
            json!("8.000000")
        );

        // sub
        assert_eq!(
            engine.evaluate("sub(10, 3)", &vars).unwrap(),
            json!("7.000000")
        );

        // multiply (used by 189天翼云: multiply(timestamp('float'), 1000))
        let result = engine
            .evaluate("multiply(timestamp('float'), 1000)", &vars)
            .unwrap();
        let text = result.as_str().unwrap();
        assert!(
            regex::Regex::new(r"^\d+\.\d{6}$").unwrap().is_match(text),
            "multiply output: {text}"
        );

        // divide
        assert_eq!(
            engine.evaluate("divide(10, 2)", &vars).unwrap(),
            json!("5.000000")
        );

        // division by zero yields None (QD returns None, not an exception)
        assert_eq!(
            engine.evaluate("divide(10, 0)", &vars).unwrap(),
            json!(null)
        );

        // non-numeric first argument yields int 0
        assert_eq!(engine.evaluate("add('abc', 1)", &vars).unwrap(), json!(0));

        // is_num
        assert_eq!(
            engine.evaluate("is_num('123')", &vars).unwrap(),
            json!(true)
        );
        assert_eq!(
            engine.evaluate("is_num('12.5')", &vars).unwrap(),
            json!(true)
        );
        assert_eq!(
            engine.evaluate("is_num('abc')", &vars).unwrap(),
            json!(false)
        );
    }

    #[test]
    fn test_regex_functions() {
        let engine = QdExpressionEngine::default();
        let vars = BTreeMap::new();

        // regex_replace (QD order: value, pattern, replacement)
        assert_eq!(
            engine
                .evaluate("regex_replace('test123foo456', '\\\\d+', 'NUM')", &vars)
                .unwrap(),
            json!("testNUMfooNUM")
        );

        // regex_search returns the matched text (QD str(match.group()))
        assert_eq!(
            engine
                .evaluate("regex_search('test123', '\\\\d+')", &vars)
                .unwrap(),
            json!("123")
        );
        // no match -> None (QD returns None implicitly)
        assert_eq!(
            engine
                .evaluate("regex_search('test', '\\\\d+')", &vars)
                .unwrap(),
            json!(null)
        );
        // backref support: \\1 / \\g<1> (QD returns str(list(groups)))
        assert_eq!(
            engine
                .evaluate("regex_search('te123foo', 'te(\\\\d+)', '\\\\1')", &vars)
                .unwrap(),
            json!("['123']")
        );
        assert_eq!(
            engine
                .evaluate("regex_search('te123foo', 'te(\\\\d+)', '\\\\g<1>')", &vars)
                .unwrap(),
            json!("['123']")
        );

        // regex_findall returns str(list) like Python re.findall
        assert_eq!(
            engine
                .evaluate("regex_findall('a1b22c333', '\\\\d+')", &vars)
                .unwrap(),
            json!("['1', '22', '333']")
        );

        // regex_escape
        assert_eq!(
            engine.evaluate("regex_escape('a.b+c*')", &vars).unwrap(),
            json!("a\\.b\\+c\\*")
        );
    }

    #[test]
    fn test_uuid_generation() {
        let engine = QdExpressionEngine::default();
        let vars = BTreeMap::new();

        // UUID v5 with DNS namespace
        let dns_namespace = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
        let result = engine
            .evaluate(
                &format!("to_uuid('example.com', '{}')", dns_namespace),
                &vars,
            )
            .unwrap();
        assert_eq!(
            result.as_str().unwrap(),
            "cfbff0d1-9375-5685-968c-48ce8b15ae17"
        );

        // UUID v5 should be deterministic
        let result2 = engine
            .evaluate(
                &format!("to_uuid('example.com', '{}')", dns_namespace),
                &vars,
            )
            .unwrap();
        assert_eq!(result, result2);
    }

    #[test]
    fn test_random_functions() {
        let engine = QdExpressionEngine::default();
        let vars = BTreeMap::new();

        // random_int
        let result = engine.evaluate("random_int(1, 10)", &vars).unwrap();
        let num = result.as_i64().unwrap();
        assert!((1..=10).contains(&num));

        // random_float
        let result = engine.evaluate("random_float(0.0, 1.0)", &vars).unwrap();
        let num = result.as_f64().unwrap();
        assert!((0.0..=1.0).contains(&num));

        // fake - just verify they return non-empty strings
        let categories = vec![
            "name",
            "first_name",
            "last_name",
            "email",
            "username",
            "password",
            "ipv4",
            "ipv6",
            "user_agent",
            "company",
            "city",
            "country",
            "phone",
        ];

        for category in categories {
            let result = engine
                .evaluate(&format!("fake('{}')", category), &vars)
                .unwrap();
            assert!(
                !result.as_str().unwrap().is_empty(),
                "fake('{}') returned empty",
                category
            );
        }

        // fake with invalid category should error
        assert!(engine.evaluate("fake('invalid_category')", &vars).is_err());
    }

    #[test]
    fn test_utility_functions() {
        let engine = QdExpressionEngine::default();
        let vars = BTreeMap::new();

        // ternary
        assert_eq!(
            engine
                .evaluate("ternary(true, 'yes', 'no')", &vars)
                .unwrap(),
            json!("yes")
        );
        assert_eq!(
            engine
                .evaluate("ternary(false, 'yes', 'no')", &vars)
                .unwrap(),
            json!("no")
        );

        // type_debug
        assert_eq!(
            engine.evaluate("type_debug('hello')", &vars).unwrap(),
            json!("string")
        );
        assert_eq!(
            engine.evaluate("type_debug(123)", &vars).unwrap(),
            json!("int")
        );
        assert_eq!(
            engine.evaluate("type_debug(12.5)", &vars).unwrap(),
            json!("float")
        );
        assert_eq!(
            engine.evaluate("type_debug(true)", &vars).unwrap(),
            json!("bool")
        );
        assert_eq!(
            engine.evaluate("type_debug([1, 2, 3])", &vars).unwrap(),
            json!("list")
        );

        // lipsum - default 1 sentence
        let result = engine.evaluate("lipsum()", &vars).unwrap();
        let text = result.as_str().unwrap();
        assert!(text.starts_with("Lorem ipsum"));
        assert!(text.ends_with('.'));

        // lipsum - multiple sentences
        let result = engine.evaluate("lipsum(2)", &vars).unwrap();
        let text = result.as_str().unwrap();
        assert!(text.len() > 100);
        assert!(text.ends_with('.'));

        // mandatory with defined value
        let mut vars_with_val = BTreeMap::new();
        vars_with_val.insert("myvar".to_string(), json!("test"));
        assert_eq!(
            engine.evaluate("mandatory(myvar)", &vars_with_val).unwrap(),
            json!("test")
        );

        // mandatory with undefined value - minijinja treats undefined variables as errors
        // so we test with null value instead
        let mut vars_with_null = BTreeMap::new();
        vars_with_null.insert("null_var".to_string(), json!(null));
        let result = engine.evaluate("mandatory(null_var)", &vars_with_null);
        assert!(result.is_err());

        // mandatory with custom error message on null
        let result = engine.evaluate("mandatory(null_var, 'Custom error')", &vars_with_null);
        assert!(result.is_err());
    }

    /// Issue #42: a template migrated from QD calls `urlencode(value, encoding,
    /// for_qs)` — QD's `urlencode` *is* `urlencode_with_encoding`, which has all
    /// three parameters — so the extra arguments have to be accepted or every
    /// migrated template has to be edited by hand.
    #[test]
    fn urlencode_takes_the_charset_and_for_qs() {
        let engine = QdExpressionEngine::default();
        let variables = BTreeMap::from([("path".to_string(), json!("a/b c"))]);

        // Without for_qs, "/" stays literal: that is QD's default and what the
        // engine has always done.
        assert_eq!(
            engine.render("{{ path|urlencode }}", &variables).unwrap(),
            "a/b%20c"
        );
        // for_qs quotes it — by keyword, which is how the reporter's templates
        // write it, and by position, which Python also allows.
        assert_eq!(
            engine
                .render("{{ path|urlencode(for_qs=True) }}", &variables)
                .unwrap(),
            "a%2Fb%20c"
        );
        assert_eq!(
            engine
                .render("{{ urlencode(path, 'utf-8', true) }}", &variables)
                .unwrap(),
            "a%2Fb%20c"
        );
        // The charset on its own is accepted in any case spelling.
        assert_eq!(
            engine
                .render("{{ urlencode(path, 'UTF-8') }}", &variables)
                .unwrap(),
            "a/b%20c"
        );
    }

    /// QD's `urlencode` also builds a query string when handed a mapping (or an
    /// iterable of pairs), quoting both sides with "/" included.
    #[test]
    fn urlencode_builds_a_query_string_from_a_mapping() {
        let engine = QdExpressionEngine::default();
        let variables = BTreeMap::from([("params".to_string(), json!({"q": "a/b", "page": 2}))]);
        assert_eq!(
            engine.render("{{ params|urlencode }}", &variables).unwrap(),
            "page=2&q=a%2Fb"
        );
    }

    /// A wrong `encoding` and an extra argument are refused with a message that
    /// says which call it was — the opposite of the bare "too many arguments"
    /// that issue #42 was filed about.
    #[test]
    fn urlencode_refuses_what_it_cannot_encode_faithfully() {
        let engine = QdExpressionEngine::default();
        let variables = BTreeMap::new();

        let err = format!(
            "{:#}",
            engine
                .render("{{ 'x'|urlencode('gbk') }}", &variables)
                .unwrap_err()
        );
        assert!(err.contains("only supports utf-8"), "{err}");

        let err = format!(
            "{:#}",
            engine
                .render("{{ 'x'|urlencode('utf-8', true, 'extra') }}", &variables)
                .unwrap_err()
        );
        assert!(err.contains("too many arguments"), "{err}");

        let err = format!(
            "{:#}",
            engine
                .render("{{ 'x'|urlencode(safe='/') }}", &variables)
                .unwrap_err()
        );
        assert!(err.contains("unexpected keyword argument"), "{err}");
    }

    /// Issue #42, second half: Jinja2's `default` takes a `boolean` argument, so
    /// `{{ x|default('y', boolean=True) }}` renders in QD and must render here.
    #[test]
    fn default_takes_jinja2s_boolean_argument() {
        let engine = QdExpressionEngine::default();
        let variables = BTreeMap::from([
            ("empty".to_string(), json!("")),
            ("set".to_string(), json!("value")),
        ]);

        // Without boolean, only a missing value is replaced — an empty string is
        // a value.
        assert_eq!(
            engine
                .render("{{ empty|default('fallback') }}", &variables)
                .unwrap(),
            ""
        );
        // With it, an empty string is falsey and is replaced too, whether the
        // flag arrives by keyword or by position.
        assert_eq!(
            engine
                .render("{{ empty|default('fallback', boolean=True) }}", &variables)
                .unwrap(),
            "fallback"
        );
        assert_eq!(
            engine
                .render("{{ empty|default('fallback', true) }}", &variables)
                .unwrap(),
            "fallback"
        );
        // …and a value that is not falsey still wins.
        assert_eq!(
            engine
                .render("{{ set|default('fallback', boolean=True) }}", &variables)
                .unwrap(),
            "value"
        );
        // `d` is Jinja2's alias and takes the same arguments.
        assert_eq!(
            engine
                .render("{{ empty|d('fallback', boolean=True) }}", &variables)
                .unwrap(),
            "fallback"
        );
    }

    /// Issue #42, third part: a render failure has to say *where*. MiniJinja
    /// knows the line, but only the alternate formatter prints it, and the run
    /// log is the only place this message is ever read from.
    #[test]
    fn a_failed_render_carries_the_line_that_failed() {
        let engine = QdExpressionEngine::default();
        let err = engine
            .render(
                "first line\n{{ 'x'|urlencode('a', 'b', 'c') }}\n",
                &BTreeMap::new(),
            )
            .unwrap_err();
        let rendered = format!("{err:#}");

        assert!(rendered.contains("too many arguments"), "{rendered}");
        // The offending line, and not just the anonymous "<string>".
        assert!(
            rendered.contains("{{ 'x'|urlencode('a', 'b', 'c') }}"),
            "{rendered}"
        );
        assert!(rendered.contains("first line"), "{rendered}");
    }
}
