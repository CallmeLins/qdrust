use std::collections::BTreeMap;

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{Local, TimeZone};
use fake::Fake;
use minijinja::{Environment, Error, ErrorKind, Value as JinjaValue};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use rand::Rng;
use regex::Regex;
use serde_json::Value;
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use uuid::Uuid;

pub struct QdExpressionEngine {
    environment: Environment<'static>,
}

impl Default for QdExpressionEngine {
    fn default() -> Self {
        let mut environment = Environment::new();

        // Type conversion functions
        environment.add_function("int", |value: JinjaValue| parse_i64(&value));
        environment.add_function("float", |value: JinjaValue| parse_f64(&value));
        environment.add_function("bool", |value: JinjaValue| value.is_true());
        environment.add_function("list", |value: JinjaValue| {
            Ok::<_, Error>(JinjaValue::from_iter(value.try_iter()?))
        });
        environment.add_function("len", |value: JinjaValue| {
            value
                .len()
                .ok_or_else(|| Error::new(ErrorKind::InvalidOperation, "value has no length"))
        });

        // Encoding functions - base64
        environment.add_function("b64encode", |value: JinjaValue| {
            let s = value.to_string();
            Ok::<_, Error>(BASE64.encode(s.as_bytes()))
        });
        environment.add_function("b64decode", |value: JinjaValue| {
            let s = value.to_string();
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

        // Encoding functions - hex
        environment.add_function("b2a_hex", |value: JinjaValue| {
            let s = value.to_string();
            Ok::<_, Error>(hex::encode(s.as_bytes()))
        });
        environment.add_function("a2b_hex", |value: JinjaValue| {
            let s = value.to_string();
            hex::decode(&s)
                .map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidOperation,
                        format!("hex decode failed: {e}"),
                    )
                })
                .and_then(|bytes| {
                    String::from_utf8(bytes).map_err(|e| {
                        Error::new(ErrorKind::InvalidOperation, format!("invalid UTF-8: {e}"))
                    })
                })
        });

        // URL encoding
        environment.add_function("urlencode", |value: JinjaValue| {
            let s = value.to_string();
            const FRAGMENT: &AsciiSet = &NON_ALPHANUMERIC
                .remove(b'-')
                .remove(b'_')
                .remove(b'.')
                .remove(b'~');
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

        // Unicode conversion (HTML unescape basic)
        environment.add_function("unicode", |value: JinjaValue| {
            let s = value.to_string();
            Ok::<_, Error>(html_escape::decode_html_entities(&s).to_string())
        });

        // Hash functions
        environment.add_function("md5", |value: JinjaValue| {
            let s = value.to_string();
            let digest = md5::compute(s.as_bytes());
            Ok::<_, Error>(format!("{:x}", digest))
        });
        environment.add_function("sha1", |value: JinjaValue| {
            use sha1::Digest;
            let s = value.to_string();
            let digest = Sha1::digest(s.as_bytes());
            Ok::<_, Error>(hex::encode(digest))
        });
        environment.add_function("hash", |value: JinjaValue, hashtype: Option<String>| {
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
        });

        // Time functions
        environment.add_function("timestamp", |type_str: Option<String>| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap();
            match type_str.as_deref() {
                Some("float") => Ok::<_, Error>(JinjaValue::from(now.as_secs_f64())),
                _ => Ok::<_, Error>(JinjaValue::from(now.as_secs())),
            }
        });

        environment.add_function(
            "date_time",
            |date: Option<JinjaValue>,
             time: Option<JinjaValue>,
             time_difference: Option<JinjaValue>| {
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
            },
        );

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

        // Math operations
        environment.add_function("add", |a: JinjaValue, b: JinjaValue| {
            let a_val = parse_f64(&a)?;
            let b_val = parse_f64(&b)?;
            Ok::<_, Error>(a_val + b_val)
        });

        environment.add_function("sub", |a: JinjaValue, b: JinjaValue| {
            let a_val = parse_f64(&a)?;
            let b_val = parse_f64(&b)?;
            Ok::<_, Error>(a_val - b_val)
        });

        environment.add_function("multiply", |a: JinjaValue, b: JinjaValue| {
            let a_val = parse_f64(&a)?;
            let b_val = parse_f64(&b)?;
            Ok::<_, Error>(a_val * b_val)
        });

        environment.add_function("divide", |a: JinjaValue, b: JinjaValue| {
            let a_val = parse_f64(&a)?;
            let b_val = parse_f64(&b)?;
            if b_val == 0.0 {
                return Err(Error::new(ErrorKind::InvalidOperation, "division by zero"));
            }
            Ok::<_, Error>(a_val / b_val)
        });

        environment.add_function("is_num", |value: JinjaValue| {
            Ok::<_, Error>(value.to_string().parse::<f64>().is_ok())
        });

        // Regex functions
        environment.add_function(
            "regex_replace",
            |pattern: String, repl: String, string: JinjaValue| {
                let s = string.to_string();
                let re = Regex::new(&pattern).map_err(|e| {
                    Error::new(ErrorKind::InvalidOperation, format!("invalid regex: {e}"))
                })?;
                Ok::<_, Error>(re.replace_all(&s, repl.as_str()).to_string())
            },
        );

        environment.add_function("regex_search", |pattern: String, string: JinjaValue| {
            let s = string.to_string();
            let re = Regex::new(&pattern).map_err(|e| {
                Error::new(ErrorKind::InvalidOperation, format!("invalid regex: {e}"))
            })?;
            Ok::<_, Error>(re.is_match(&s))
        });

        environment.add_function("regex_findall", |pattern: String, string: JinjaValue| {
            let s = string.to_string();
            let re = Regex::new(&pattern).map_err(|e| {
                Error::new(ErrorKind::InvalidOperation, format!("invalid regex: {e}"))
            })?;
            let matches: Vec<String> = re.find_iter(&s).map(|m| m.as_str().to_string()).collect();
            Ok::<_, Error>(JinjaValue::from_iter(matches))
        });

        environment.add_function("regex_escape", |string: JinjaValue| {
            Ok::<_, Error>(regex::escape(&string.to_string()))
        });

        // UUID generation
        environment.add_function("to_uuid", |name: JinjaValue, namespace: Option<String>| {
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
        });

        // Random value generation
        environment.add_function("random_int", |min: i64, max: i64| {
            let mut rng = rand::thread_rng();
            Ok::<_, Error>(rng.gen_range(min..=max))
        });

        environment.add_function("random_float", |min: f64, max: f64| {
            let mut rng = rand::thread_rng();
            Ok::<_, Error>(rng.gen_range(min..=max))
        });

        environment.add_function("fake", |category: String| {
            use fake::faker::address::en::*;
            use fake::faker::company::en::*;
            use fake::faker::internet::en::*;
            use fake::faker::name::en::*;
            use fake::faker::phone_number::en::*;

            let result: String = match category.as_str() {
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
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidOperation,
                        format!("unknown fake category: {}", category),
                    ));
                }
            };
            Ok::<_, Error>(result)
        });

        // Utility functions
        environment.add_function(
            "ternary",
            |condition: bool, true_val: JinjaValue, false_val: JinjaValue| {
                Ok::<_, Error>(if condition { true_val } else { false_val })
            },
        );

        environment.add_function("mandatory", |value: JinjaValue, msg: Option<String>| {
            if value.is_undefined() || value.is_none() {
                let error_msg =
                    msg.unwrap_or_else(|| "Mandatory variable is undefined".to_string());
                Err(Error::new(ErrorKind::UndefinedError, error_msg))
            } else {
                Ok::<_, Error>(value)
            }
        });

        environment.add_function("type_debug", |value: JinjaValue| {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

        // hex
        assert_eq!(
            engine.evaluate("b2a_hex('hi')", &vars).unwrap(),
            json!("6869")
        );
        assert_eq!(
            engine.evaluate("a2b_hex('6869')", &vars).unwrap(),
            json!("hi")
        );

        // urlencode
        assert_eq!(
            engine.evaluate("urlencode('hello world')", &vars).unwrap(),
            json!("hello%20world")
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

        // add
        assert_eq!(engine.evaluate("add(5, 3)", &vars).unwrap(), json!(8.0));
        assert_eq!(
            engine.evaluate("add('5.5', '2.5')", &vars).unwrap(),
            json!(8.0)
        );

        // sub
        assert_eq!(engine.evaluate("sub(10, 3)", &vars).unwrap(), json!(7.0));

        // multiply
        assert_eq!(
            engine.evaluate("multiply(4, 5)", &vars).unwrap(),
            json!(20.0)
        );

        // divide
        assert_eq!(engine.evaluate("divide(10, 2)", &vars).unwrap(), json!(5.0));

        // division by zero
        assert!(engine.evaluate("divide(10, 0)", &vars).is_err());

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

        // regex_replace
        assert_eq!(
            engine
                .evaluate("regex_replace('\\\\d+', 'NUM', 'test123foo456')", &vars)
                .unwrap(),
            json!("testNUMfooNUM")
        );

        // regex_search
        assert_eq!(
            engine
                .evaluate("regex_search('\\\\d+', 'test123')", &vars)
                .unwrap(),
            json!(true)
        );
        assert_eq!(
            engine
                .evaluate("regex_search('\\\\d+', 'test')", &vars)
                .unwrap(),
            json!(false)
        );

        // regex_findall
        assert_eq!(
            engine
                .evaluate("regex_findall('\\\\d+', 'a1b22c333')", &vars)
                .unwrap(),
            json!(["1", "22", "333"])
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
