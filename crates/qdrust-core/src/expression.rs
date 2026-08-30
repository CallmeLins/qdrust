use std::collections::BTreeMap;

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{Local, TimeZone};
use fake::Fake;
use minijinja::value::{Kwargs, Rest};
use minijinja::{Environment, Error, ErrorKind, Value as JinjaValue};
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

pub struct QdExpressionEngine {
    environment: Environment<'static>,
}

impl Default for QdExpressionEngine {
    fn default() -> Self {
        let mut environment = Environment::new();

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

        // URL encoding (QD urlencode = urllib.parse.quote with safe="/",
        // so "/" stays literal and space becomes %20, not +).
        qd_fn!(environment, "urlencode", |value: JinjaValue| {
            let s = value.to_string();
            const FRAGMENT: &AsciiSet = &NON_ALPHANUMERIC
                .remove(b'-')
                .remove(b'_')
                .remove(b'.')
                .remove(b'~')
                .remove(b'/');
            Ok::<_, Error>(utf8_percent_encode(&s, FRAGMENT).to_string())
        });
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

        // Utility filters
        environment.add_filter("default", |value: JinjaValue, default_value: JinjaValue| {
            if value.is_undefined() || value.is_none() {
                Ok::<_, Error>(default_value)
            } else {
                Ok(value)
            }
        });

        environment.add_filter("d", |value: JinjaValue, default_value: JinjaValue| {
            if value.is_undefined() || value.is_none() {
                Ok::<_, Error>(default_value)
            } else {
                Ok(value)
            }
        });

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

        Self { environment }
    }
}

impl QdExpressionEngine {
    pub fn evaluate(&self, expression: &str, variables: &BTreeMap<String, Value>) -> Result<Value> {
        let compiled = self
            .environment
            .compile_expression(expression)
            .context("invalid QD expression")?;
        let value = compiled
            .eval(variables)
            .context("cannot evaluate QD expression")?;
        serde_json::to_value(value).context("cannot convert QD expression result")
    }

    /// Render a template string with the full QD function/filter set. Used by
    /// the server for non-template tasks so `{{ var }}` and qd functions such
    /// as `{{ md5(x) }}` work in plain task URLs, headers and bodies too.
    pub fn render(&self, template: &str, variables: &BTreeMap<String, Value>) -> Result<String> {
        self.environment
            .render_str(template, variables)
            .context("cannot render QD template value")
    }

    pub fn evaluate_bool(
        &self,
        expression: &str,
        variables: &BTreeMap<String, Value>,
    ) -> Result<bool> {
        let compiled = self
            .environment
            .compile_expression(expression)
            .context("invalid QD condition")?;
        Ok(compiled
            .eval(variables)
            .context("cannot evaluate QD condition")?
            .is_true())
    }
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
                    let pad_char = if zero_pad && !align.is_some() {
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
}
