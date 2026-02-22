//! jq expression evaluator for Serverless Workflow runtime expressions.
//!
//! The spec uses `${ <jq> }` syntax. This module wraps jaq to provide
//! a simple evaluate/evaluate_bool facade.

use jaq_interpret::FilterT;
use serde_json::Value;

/// Expression evaluation errors.
#[derive(Debug, thiserror::Error)]
pub enum ExprError {
    #[error("parse error in expression '{filter}': {message}")]
    Parse { filter: String, message: String },
    #[error("evaluation error: {0}")]
    Eval(String),
    #[error("expression produced no output: {0}")]
    NoOutput(String),
}

/// Check if a string is a runtime expression (`${ ... }`).
pub fn is_runtime_expr(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("${") && t.ends_with('}')
}

/// Extract the inner jq expression from `${ ... }` wrapper.
/// Returns the original string if it's not a runtime expression.
pub fn unwrap_expr(s: &str) -> &str {
    let t = s.trim();
    if t.starts_with("${") && t.ends_with('}') {
        t[2..t.len() - 1].trim()
    } else {
        s
    }
}

/// Evaluate a jq expression against a JSON value.
///
/// `input` is `.` (identity) in jq — the root value the filter operates on.
/// `vars` are named variables available as `$name` in the expression.
///
/// Returns the first output value (jq filters can produce multiple outputs).
pub fn evaluate(expr: &str, input: &Value, vars: &[(&str, Value)]) -> Result<Value, ExprError> {
    let filter = unwrap_expr(expr);
    let compiled = compile(filter, vars)?;

    let inputs = jaq_interpret::RcIter::new(core::iter::empty());
    let var_values = vars.iter().map(|(_, v)| jaq_interpret::Val::from(v.clone()));
    let ctx = jaq_interpret::Ctx::new(var_values, &inputs);

    let mut out = compiled.run((ctx, jaq_interpret::Val::from(input.clone())));

    match out.next() {
        Some(Ok(val)) => Ok(Value::from(val)),
        Some(Err(e)) => Err(ExprError::Eval(format!("{e}"))),
        None => Err(ExprError::NoOutput(filter.to_string())),
    }
}

/// Evaluate a jq expression and collect all outputs.
pub fn evaluate_all(
    expr: &str,
    input: &Value,
    vars: &[(&str, Value)],
) -> Result<Vec<Value>, ExprError> {
    let filter = unwrap_expr(expr);
    let compiled = compile(filter, vars)?;

    let inputs = jaq_interpret::RcIter::new(core::iter::empty());
    let var_values = vars.iter().map(|(_, v)| jaq_interpret::Val::from(v.clone()));
    let ctx = jaq_interpret::Ctx::new(var_values, &inputs);

    let out = compiled.run((ctx, jaq_interpret::Val::from(input.clone())));

    out.map(|r| match r {
        Ok(val) => Ok(Value::from(val)),
        Err(e) => Err(ExprError::Eval(format!("{e}"))),
    })
    .collect()
}

/// Evaluate a jq expression as a boolean condition.
///
/// Truthiness: `false` and `null` are falsy, everything else is truthy.
/// (Same as jq semantics.)
pub fn evaluate_bool(
    expr: &str,
    input: &Value,
    vars: &[(&str, Value)],
) -> Result<bool, ExprError> {
    let result = evaluate(expr, input, vars)?;
    Ok(is_truthy(&result))
}

/// Resolve a value that may contain runtime expressions.
///
/// - If the value is a string starting with `${`, evaluate it as jq.
/// - Otherwise return the value as-is.
pub fn resolve_value(
    value: &Value,
    input: &Value,
    vars: &[(&str, Value)],
) -> Result<Value, ExprError> {
    match value {
        Value::String(s) if is_runtime_expr(s) => evaluate(s, input, vars),
        other => Ok(other.clone()),
    }
}

/// Resolve all string values in a JSON object that contain runtime expressions.
pub fn resolve_object(
    obj: &Value,
    input: &Value,
    vars: &[(&str, Value)],
) -> Result<Value, ExprError> {
    match obj {
        Value::Object(map) => {
            let mut result = serde_json::Map::new();
            for (k, v) in map {
                result.insert(k.clone(), resolve_value(v, input, vars)?);
            }
            Ok(Value::Object(result))
        }
        other => resolve_value(other, input, vars),
    }
}

// -- internals --

fn is_truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
}

fn compile(
    filter: &str,
    vars: &[(&str, Value)],
) -> Result<jaq_interpret::Filter, ExprError> {
    let var_names: Vec<String> = vars.iter().map(|(name, _)| name.to_string()).collect();
    let mut ctx = jaq_interpret::ParseCtx::new(var_names);

    // Load core native filters (length, keys, type, etc.)
    ctx.insert_natives(jaq_core::core());
    // Load standard library (map, select, first, last, etc.)
    ctx.insert_defs(jaq_std::std());

    let (parsed, errs) = jaq_parse::parse(filter, jaq_parse::main());

    if !errs.is_empty() {
        let messages: Vec<String> = errs.iter().map(|e| format!("{e:?}")).collect();
        return Err(ExprError::Parse {
            filter: filter.to_string(),
            message: messages.join("; "),
        });
    }

    let parsed = parsed.ok_or_else(|| ExprError::Parse {
        filter: filter.to_string(),
        message: "parser returned no output".to_string(),
    })?;

    let compiled = ctx.compile(parsed);

    if !ctx.errs.is_empty() {
        return Err(ExprError::Parse {
            filter: filter.to_string(),
            message: format!("{} compilation error(s)", ctx.errs.len()),
        });
    }

    Ok(compiled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identity() {
        let input = json!({"name": "test"});
        let result = evaluate(".", &input, &[]).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn field_access() {
        let input = json!({"name": "nexus", "version": 1});
        let result = evaluate(".name", &input, &[]).unwrap();
        assert_eq!(result, json!("nexus"));
    }

    #[test]
    fn nested_field() {
        let input = json!({"data": {"repo": {"name": "nexus-server"}}});
        let result = evaluate(".data.repo.name", &input, &[]).unwrap();
        assert_eq!(result, json!("nexus-server"));
    }

    #[test]
    fn runtime_expr_wrapper() {
        let input = json!({"x": 42});
        let result = evaluate("${ .x }", &input, &[]).unwrap();
        assert_eq!(result, json!(42));
    }

    #[test]
    fn is_runtime_expr_detection() {
        assert!(is_runtime_expr("${ .field }"));
        assert!(is_runtime_expr("${.field}"));
        assert!(is_runtime_expr("  ${ .x }  "));
        assert!(!is_runtime_expr(".field"));
        assert!(!is_runtime_expr("hello"));
        assert!(!is_runtime_expr("$field"));
    }

    #[test]
    fn named_variables() {
        let input = json!({"x": 1});
        let vars = [("context", json!({"count": 42}))];
        let result = evaluate("$context.count", &input, &vars).unwrap();
        assert_eq!(result, json!(42));
    }

    #[test]
    fn condition_truthy() {
        let input = json!({"status": "open"});
        assert!(evaluate_bool(".status == \"open\"", &input, &[]).unwrap());
        assert!(!evaluate_bool(".status == \"closed\"", &input, &[]).unwrap());
    }

    #[test]
    fn condition_null_is_falsy() {
        let input = json!(null);
        assert!(!evaluate_bool(".", &input, &[]).unwrap());
    }

    #[test]
    fn pipe_and_length() {
        let input = json!({"items": [1, 2, 3]});
        let result = evaluate(".items | length", &input, &[]).unwrap();
        assert_eq!(result, json!(3));
    }

    #[test]
    fn string_interpolation() {
        let input = json!({"name": "world"});
        let result = evaluate(r#""hello \(.name)""#, &input, &[]).unwrap();
        assert_eq!(result, json!("hello world"));
    }

    #[test]
    fn resolve_value_expr() {
        let input = json!({"x": 10});
        let val = json!("${ .x + 5 }");
        let result = resolve_value(&val, &input, &[]).unwrap();
        assert_eq!(result, json!(15));
    }

    #[test]
    fn resolve_value_literal() {
        let input = json!({});
        let val = json!("just a string");
        let result = resolve_value(&val, &input, &[]).unwrap();
        assert_eq!(result, json!("just a string"));
    }

    #[test]
    fn resolve_object_mixed() {
        let input = json!({"name": "nexus"});
        let obj = json!({
            "greeting": "${ \"hello \\(.name)\" }",
            "static_field": "unchanged",
            "number": 42
        });
        let result = resolve_object(&obj, &input, &[]).unwrap();
        assert_eq!(result["greeting"], json!("hello nexus"));
        assert_eq!(result["static_field"], json!("unchanged"));
        assert_eq!(result["number"], json!(42));
    }

    #[test]
    fn if_then_else() {
        let input = json!({"count": 5});
        let result = evaluate(
            "if .count > 3 then \"many\" else \"few\" end",
            &input,
            &[],
        )
        .unwrap();
        assert_eq!(result, json!("many"));
    }

    #[test]
    fn evaluate_all_multiple_outputs() {
        let input = json!([1, 2, 3]);
        let results = evaluate_all(".[]", &input, &[]).unwrap();
        assert_eq!(results, vec![json!(1), json!(2), json!(3)]);
    }

    #[test]
    fn parse_error() {
        let input = json!({});
        let result = evaluate(".[invalid syntax", &input, &[]);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ExprError::Parse { .. }));
    }
}
