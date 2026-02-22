use crate::config::RuleConfig;
use cloudevents::event::AttributesReader;
use cloudevents::Event;
use regex::Regex;

/// Return all rules whose filter matches the event type.
pub fn match_rules<'a>(
    event: &Event,
    rules: &'a [RuleConfig],
) -> Vec<&'a RuleConfig> {
    rules
        .iter()
        .filter(|r| r.enabled && event.ty().starts_with(&r.filter.type_prefix))
        .collect()
}

/// Resolve `{{event.data.x.y}}` template expressions against the event.
///
/// Supports dotted paths into the JSON data. Missing paths resolve to an empty string.
pub fn resolve_template(template: &str, event: &Event) -> String {
    let re = Regex::new(r"\{\{event\.data\.([^}]+)\}\}").unwrap();
    re.replace_all(template, |caps: &regex::Captures| {
        let path = &caps[1];
        let json_data = match event.data() {
            Some(cloudevents::Data::Json(v)) => v,
            _ => return String::new(),
        };
        resolve_json_path(json_data, path)
    })
    .into_owned()
}

fn resolve_json_path(value: &serde_json::Value, path: &str) -> String {
    let mut current = value;
    for segment in path.split('.') {
        match current.get(segment) {
            Some(v) => current = v,
            None => return String::new(),
        }
    }
    match current {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FilterConfig;
    use cloudevents::{EventBuilder, EventBuilderV10};
    use serde_json::json;

    fn make_event(type_: &str, data: serde_json::Value) -> Event {
        EventBuilderV10::new()
            .id(uuid::Uuid::new_v4().to_string())
            .ty(type_)
            .source("test")
            .data("application/json", data)
            .build()
            .expect("valid test event")
    }

    fn make_rule(name: &str, prefix: &str, action: &str) -> RuleConfig {
        RuleConfig {
            name: name.to_string(),
            filter: FilterConfig {
                type_prefix: prefix.to_string(),
            },
            action: action.to_string(),
            enabled: true,
            prompt: None,
            system_prompt: None,
            url: None,
            body_template: None,
            target: None,
            target_secret_env: None,
            workflow: None,
        }
    }

    #[test]
    fn match_by_prefix() {
        let rules = vec![
            make_rule("issues", "com.github.issues", "claude"),
            make_rule("push", "com.github.push", "http_post"),
            make_rule("all github", "com.github", "http_post"),
        ];
        let event = make_event("com.github.issues.opened", json!({}));
        let matched = match_rules(&event, &rules);
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].name, "issues");
        assert_eq!(matched[1].name, "all github");
    }

    #[test]
    fn no_match() {
        let rules = vec![make_rule("push", "com.github.push", "http_post")];
        let event = make_event("com.gitlab.merge_request", json!({}));
        assert!(match_rules(&event, &rules).is_empty());
    }

    #[test]
    fn resolve_nested_path() {
        let event = make_event(
            "com.github.issues.opened",
            json!({
                "issue": {
                    "title": "Bug report",
                    "user": { "login": "dani" }
                },
                "repository": { "full_name": "org/repo" }
            }),
        );
        let template = "Issue: {{event.data.issue.title}} by {{event.data.issue.user.login}} in {{event.data.repository.full_name}}";
        let result = resolve_template(template, &event);
        assert_eq!(result, "Issue: Bug report by dani in org/repo");
    }

    #[test]
    fn missing_path_resolves_empty() {
        let event = make_event("test", json!({"a": 1}));
        let result = resolve_template("val={{event.data.missing.path}}", &event);
        assert_eq!(result, "val=");
    }

    #[test]
    fn numeric_values() {
        let event = make_event("test", json!({"count": 42, "active": true}));
        let result =
            resolve_template("{{event.data.count}} {{event.data.active}}", &event);
        assert_eq!(result, "42 true");
    }
}
