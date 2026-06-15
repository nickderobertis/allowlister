//! The published JSON Schema (`schema/allowlister.schema.json`) is the contract
//! editors validate config files against, so it must stay in lockstep with the
//! values the loader actually accepts. These tests fail the moment the schema's
//! enumerations drift from the engine's `parse` functions, or the canonical
//! `$id` changes out from under the documentation and the example configs.

use std::collections::BTreeSet;
use std::path::PathBuf;

use allowlister::domain::{ParamKey, Role};
use serde_json::Value;

/// The canonical, publicly hosted location of the schema. Editors and tooling
/// reference this exact string; the example configs embed it as `"$schema"`.
const SCHEMA_ID: &str = "https://nickderobertis.github.io/allowlister/allowlister.schema.json";

fn schema() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema/allowlister.schema.json");
    let text = std::fs::read_to_string(&path).expect("schema file is readable");
    serde_json::from_str(&text).expect("schema is well-formed JSON")
}

/// The `enum` string array at `$defs/<name>` (or, for a nested string field, the
/// `enum` directly under it).
fn enum_values(schema: &Value, pointer: &str) -> BTreeSet<String> {
    schema
        .pointer(pointer)
        .and_then(|node| node.get("enum"))
        .and_then(Value::as_array)
        .expect("node has an enum array")
        .iter()
        .map(|v| v.as_str().expect("enum entries are strings").to_string())
        .collect()
}

#[test]
fn schema_declares_canonical_id_and_draft() {
    let schema = schema();
    assert_eq!(schema["$id"].as_str(), Some(SCHEMA_ID));
    assert_eq!(
        schema["$schema"].as_str(),
        Some("https://json-schema.org/draft/2020-12/schema")
    );
}

#[test]
fn role_enum_matches_what_the_engine_parses() {
    let roles = enum_values(&schema(), "/$defs/role");
    // Every value the schema offers must be one the engine accepts...
    for role in &roles {
        assert!(
            Role::parse(role).is_some(),
            "schema lists role {role:?} that the engine rejects"
        );
    }
    // ...and the set must be exactly the engine's vocabulary, so a new role added
    // to the engine forces a matching schema update here.
    let expected: BTreeSet<String> = [
        "standalone",
        "pipe_source",
        "pipe_filter",
        "subshell",
        "substitution",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(roles, expected);
    assert!(Role::parse("not_a_role").is_none());
}

#[test]
fn param_keys_match_what_the_engine_parses() {
    // The `params` object's named properties are the canonical parameter keys.
    let props = schema()["$defs"]["params"]["properties"]
        .as_object()
        .expect("params has a properties object")
        .keys()
        .cloned()
        .collect::<BTreeSet<String>>();
    for key in &props {
        assert!(
            ParamKey::parse(key).is_some(),
            "schema lists param {key:?} that the engine rejects"
        );
    }
    let expected: BTreeSet<String> = [
        "path",
        "url",
        "query",
        "pattern",
        "content",
        "mcp_server",
        "mcp_tool",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(props, expected);
    // `params` rejects any other key, mirroring the loader.
    assert_eq!(
        schema()["$defs"]["params"]["additionalProperties"],
        Value::Bool(false)
    );
}

#[test]
fn action_kind_and_grant_enums_match_the_loader() {
    let schema = schema();
    assert_eq!(
        enum_values(&schema, "/$defs/action"),
        ["allow", "deny", "ask"]
            .iter()
            .map(ToString::to_string)
            .collect()
    );
    assert_eq!(
        enum_values(&schema, "/$defs/kind"),
        ["glob", "regex", "literal"]
            .iter()
            .map(ToString::to_string)
            .collect()
    );
    assert_eq!(
        enum_values(&schema, "/$defs/bashRule/properties/grants"),
        ["command", "redirections"]
            .iter()
            .map(ToString::to_string)
            .collect()
    );
}

#[test]
fn example_configs_reference_the_published_schema() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for name in ["examples/user-config.json", "examples/project-config.json"] {
        let text = std::fs::read_to_string(root.join(name)).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["$schema"].as_str(),
            Some(SCHEMA_ID),
            "{name} should reference the published schema"
        );
    }
}
