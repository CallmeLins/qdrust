use std::collections::BTreeMap;

use anyhow::{Context, Result};
use minijinja::{Environment, Error, ErrorKind, Value as JinjaValue};
use serde_json::Value;

pub struct QdExpressionEngine {
    environment: Environment<'static>,
}

impl Default for QdExpressionEngine {
    fn default() -> Self {
        let mut environment = Environment::new();
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
}
