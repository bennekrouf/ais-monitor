use regex::Regex;
use serde_json::{json, Value};

/// Reads a workflow.json and returns a pretty-printed sample JSON body for its trigger.
///
/// Priority (same as ais-runner):
///   1. First ParseJson action that reads @triggerBody() — stricter, causes actual failures.
///   2. Trigger-level schema — fallback when no ParseJson action is present.
///   3. Regex scan of @triggerBody()?['field'] access chains.
pub fn suggest_payload(logic_apps_dir: &str, workflow_name: &str) -> String {
    let base = std::path::Path::new(logic_apps_dir);
    // Support platform roots that contain a logic_apps/ subfolder
    let resolved = if base.join("logic_apps").exists() {
        base.join("logic_apps")
    } else if base.join("logic-apps").exists() {
        base.join("logic-apps")
    } else {
        base.to_path_buf()
    };

    let path = resolved.join(workflow_name).join("workflow.json");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return "{}".to_string(),
    };
    let workflow: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return "{}".to_string(),
    };

    let defn = workflow.get("definition").unwrap_or(&workflow);

    // Detect Service Bus trigger so we can unwrap the contentData envelope.
    let is_sb_trigger = defn["triggers"]
        .as_object()
        .and_then(|t| t.values().next())
        .map(|trigger| {
            trigger["inputs"]["serviceProviderConfiguration"]["serviceProviderId"].as_str()
                == Some("/serviceProviders/serviceBus")
        })
        .unwrap_or(false);

    // 1. First ParseJson action that reads triggerBody / triggerOutputs / contentData.
    //    Checked before the trigger schema because it tends to be stricter (e.g. it
    //    adds `items` constraints on arrays that the trigger schema leaves open), and
    //    validation failures happen against this schema, not the trigger schema.
    if let Some(actions) = defn["actions"].as_object() {
        if let Some(schema) = find_trigger_body_schema(actions) {
            let sample = schema_to_sample("", &schema);
            if is_sb_trigger {
                if let Value::Object(ref map) = sample {
                    if map.len() == 1 {
                        if let Some(inner) = map.get("contentData") {
                            return pretty(inner.clone());
                        }
                    }
                }
            }
            return pretty(sample);
        }
    }

    // 2. Trigger-level schema — fallback when no ParseJson action is present.
    if let Some(triggers) = defn["triggers"].as_object() {
        if let Some(trigger) = triggers.values().next() {
            let schema = &trigger["inputs"]["schema"];
            if schema.is_object() && !schema.as_object().map(|m| m.is_empty()).unwrap_or(true) {
                return pretty(schema_to_sample("", schema));
            }
        }
    }

    // 3. Regex-scan the raw workflow text for @triggerBody()?['field'] access chains.
    if let Some(skeleton) = scan_trigger_body_refs(&content) {
        return pretty(skeleton);
    }

    "{}".to_string()
}

fn pretty(v: Value) -> String {
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
}

fn find_trigger_body_schema(actions: &serde_json::Map<String, Value>) -> Option<Value> {
    for (_name, action) in actions {
        if action["type"].as_str() == Some("ParseJson") {
            let content = action["inputs"]["content"].as_str().unwrap_or("");
            if content.contains("triggerBody")
                || content.contains("triggerOutputs")
                || content.contains("contentData")
                || content.contains("items(")
            {
                let schema = &action["inputs"]["schema"];
                if schema.is_object() && !schema.as_object()?.is_empty() {
                    return Some(schema.clone());
                }
            }
        }
        // Recurse into nested scopes / foreach / switch / conditions
        for sub_key in &["actions", "else", "cases", "default"] {
            if let Some(nested) = action[sub_key].as_object() {
                if let Some(s) = find_trigger_body_schema(nested) {
                    return Some(s);
                }
            }
        }
    }
    None
}

fn resolve_type(schema: &Value) -> &str {
    match &schema["type"] {
        Value::String(s) => s.as_str(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .find(|s| *s != "null")
            .unwrap_or(""),
        _ => "",
    }
}

fn schema_to_sample(field_name: &str, schema: &Value) -> Value {
    match resolve_type(schema) {
        "object" => {
            if let Some(props) = schema["properties"].as_object() {
                let mut map = serde_json::Map::new();
                for (k, v) in props {
                    map.insert(k.clone(), sample_named(k, v));
                }
                Value::Object(map)
            } else {
                sample_object_by_name(field_name)
            }
        }
        "array" => Value::Array(vec![schema_to_sample("item", &schema["items"])]),
        "integer" | "number" => Value::Number(serde_json::Number::from(0)),
        "boolean" => Value::Bool(false),
        _ => {
            if schema["properties"].is_object() {
                let mut map = serde_json::Map::new();
                if let Some(props) = schema["properties"].as_object() {
                    for (k, v) in props {
                        map.insert(k.clone(), sample_named(k, v));
                    }
                }
                Value::Object(map)
            } else {
                Value::String("text".to_string())
            }
        }
    }
}

fn sample_object_by_name(name: &str) -> Value {
    match name.to_lowercase().as_str() {
        "cloudevent" => json!({
            "specversion": "1.0",
            "type": "com.oryx.event",
            "source": "manual",
            "id": "TEST-001",
            "time": "2026-01-01T00:00:00Z",
            "data": {
                "msg": {
                    "correlationId": "TEST-001",
                    "identifier":    "TEST-001",
                    "parentId":      "",
                    "schema":        "",
                    "content":       {}
                }
            }
        }),
        "msg" => json!({
            "correlationId": "TEST-001",
            "identifier":    "TEST-001",
            "parentId":      "",
            "schema":        "",
            "content":       {}
        }),
        "data" => json!({
            "msg": {
                "correlationId": "TEST-001",
                "identifier":    "TEST-001",
                "parentId":      "",
                "content":       {}
            }
        }),
        "ais.workflow.error" => json!({
            "action": "Upload_to_Kyriba",
            "message": "The service provider action failed with error code 'EmptyFile'",
            "messageDetails": "",
            "workflowStack": [
                {
                    "name": "Send-Kyriba-files-broadcast",
                    "identifier": "TEST-RUN-001",
                    "startTime": "2026-01-01T10:00:00Z",
                    "input": { "name": "payments/test.txt" }
                }
            ]
        }),
        "connector" | "content" => Value::Object(serde_json::Map::new()),
        _ => Value::Object(serde_json::Map::new()),
    }
}

fn scan_trigger_body_refs(content: &str) -> Option<Value> {
    let chain_re = Regex::new(r"triggerBody\(\)\??(?:\[?'([^']+)'\]?\??)+").ok()?;
    let seg_re = Regex::new(r"\[?'([^']+)'\]?").ok()?;

    let mut paths: Vec<Vec<String>> = Vec::new();
    for cap in chain_re.find_iter(content) {
        let matched = cap.as_str();
        let after = &matched["triggerBody()".len()..];
        let segs: Vec<String> = seg_re
            .captures_iter(after)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect();
        if !segs.is_empty() && !paths.contains(&segs) {
            paths.push(segs);
        }
    }

    if paths.is_empty() {
        return None;
    }

    let mut root = serde_json::Map::new();
    for path in &paths {
        insert_path(&mut root, path);
    }
    Some(Value::Object(root))
}

fn insert_path(map: &mut serde_json::Map<String, Value>, path: &[String]) {
    if path.is_empty() {
        return;
    }
    let key = &path[0];
    if path.len() == 1 {
        map.entry(key.clone()).or_insert_with(|| leaf_value(key));
    } else {
        let child = map
            .entry(key.clone())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Value::Object(ref mut m) = child {
            insert_path(m, &path[1..]);
        }
    }
}

fn leaf_value(name: &str) -> Value {
    let n = name.to_lowercase();
    let s = if n.contains("error") || n.contains("message") {
        "Something went wrong"
    } else if n.contains("code") {
        "TEST-001"
    } else if n.contains("id") || n.contains("key") {
        "TEST-001"
    } else if n.contains("date") || n.contains("time") {
        "2026-04-29T10:00:00Z"
    } else {
        "text"
    };
    Value::String(s.to_string())
}

pub(crate) fn sample_named(name: &str, schema: &Value) -> Value {
    let ty = resolve_type(schema);
    if !ty.is_empty() && ty != "string" {
        return schema_to_sample(name, schema);
    }
    let n = name.to_lowercase();
    let s = if n.contains("date") || n.contains("time") {
        "2026-01-01T00:00:00Z"
    } else if n == "environment" || n.contains("env") {
        "dev"
    } else if n == "module" {
        "SageX3"
    } else if n == "source" {
        "manual"
    } else if n == "type" {
        "com.oryx.event"
    } else if n == "specversion" {
        "1.0"
    } else if n.contains("schema") {
        "CloudEvent"
    } else if n.contains("id") || n.contains("key") {
        "TEST-001"
    } else if n.contains("by") || n.contains("user") {
        "test-user"
    } else if n == "value" {
        "example"
    } else if n.contains("name") {
        name
    } else {
        "text"
    };
    Value::String(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── leaf_value ────────────────────────────────────────────────────────

    #[test]
    fn leaf_id_fields_produce_test_id() {
        assert_eq!(
            leaf_value("correlationId"),
            Value::String("TEST-001".into())
        );
        assert_eq!(leaf_value("identifier"), Value::String("TEST-001".into()));
    }

    #[test]
    fn leaf_time_fields_produce_timestamp() {
        assert_eq!(
            leaf_value("startTime"),
            Value::String("2026-04-29T10:00:00Z".into())
        );
        assert_eq!(
            leaf_value("date"),
            Value::String("2026-04-29T10:00:00Z".into())
        );
    }

    #[test]
    fn leaf_error_fields_produce_message() {
        assert_eq!(
            leaf_value("errorMessage"),
            Value::String("Something went wrong".into())
        );
        assert_eq!(
            leaf_value("error"),
            Value::String("Something went wrong".into())
        );
    }

    #[test]
    fn leaf_unknown_field_produces_text() {
        assert_eq!(leaf_value("whatever"), Value::String("text".into()));
    }

    // ── resolve_type ──────────────────────────────────────────────────────

    #[test]
    fn resolve_type_simple_string() {
        let schema = json!({ "type": "integer" });
        assert_eq!(resolve_type(&schema), "integer");
    }

    #[test]
    fn resolve_type_nullable_array_picks_non_null() {
        let schema = json!({ "type": ["null", "string"] });
        assert_eq!(resolve_type(&schema), "string");
    }

    #[test]
    fn resolve_type_missing_returns_empty() {
        assert_eq!(resolve_type(&json!({})), "");
    }

    // ── schema_to_sample ──────────────────────────────────────────────────

    #[test]
    fn schema_object_with_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age":  { "type": "integer" }
            }
        });
        let sample = schema_to_sample("root", &schema);
        assert!(sample["name"].is_string());
        assert_eq!(sample["age"], json!(0));
    }

    #[test]
    fn schema_array_wraps_item() {
        let schema = json!({ "type": "array", "items": { "type": "boolean" } });
        let sample = schema_to_sample("items", &schema);
        assert_eq!(sample, json!([false]));
    }

    #[test]
    fn schema_boolean() {
        assert_eq!(
            schema_to_sample("flag", &json!({ "type": "boolean" })),
            json!(false)
        );
    }

    #[test]
    fn schema_number() {
        assert_eq!(
            schema_to_sample("count", &json!({ "type": "number" })),
            json!(0)
        );
    }

    // ── scan_trigger_body_refs ─────────────────────────────────────────────

    #[test]
    fn scan_simple_field_reference() {
        let content = r#"@triggerBody()?['correlationId']"#;
        let result = scan_trigger_body_refs(content).unwrap();
        assert_eq!(result["correlationId"], json!("TEST-001"));
    }

    #[test]
    fn scan_nested_reference() {
        let content = r#"@triggerBody()?['data']?['msg']"#;
        let result = scan_trigger_body_refs(content).unwrap();
        // "data" is an intermediate object, "msg" is a leaf string
        assert!(result["data"].is_object());
        assert!(result["data"]["msg"].is_string());
    }

    #[test]
    fn scan_no_trigger_refs_returns_none() {
        assert!(scan_trigger_body_refs("no references here").is_none());
    }

    #[test]
    fn scan_deduplicates_paths() {
        let content = "@triggerBody()?['id'] @triggerBody()?['id']";
        let result = scan_trigger_body_refs(content).unwrap();
        let obj = result.as_object().unwrap();
        assert_eq!(obj.len(), 1);
    }

    // ── find_trigger_body_schema ──────────────────────────────────────────

    #[test]
    fn find_schema_from_parse_json_action() {
        let actions = serde_json::from_str::<serde_json::Map<String, Value>>(
            r#"{
            "Parse_body": {
                "type": "ParseJson",
                "inputs": {
                    "content": "@triggerBody()",
                    "schema": {
                        "type": "object",
                        "properties": { "id": { "type": "string" } }
                    }
                }
            }
        }"#,
        )
        .unwrap();
        assert!(find_trigger_body_schema(&actions).is_some());
    }

    #[test]
    fn find_schema_ignores_non_trigger_parse_json() {
        let actions = serde_json::from_str::<serde_json::Map<String, Value>>(
            r#"{
            "Parse_other": {
                "type": "ParseJson",
                "inputs": {
                    "content": "@body('HTTP')",
                    "schema": { "type": "object", "properties": { "x": { "type": "string" } } }
                }
            }
        }"#,
        )
        .unwrap();
        assert!(find_trigger_body_schema(&actions).is_none());
    }
}
