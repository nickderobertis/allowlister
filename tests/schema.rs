//! The published JSON Schema (`schema/allowlister.schema.json`) is the contract
//! editors validate config files against, so it must stay in lockstep with the
//! engine. The drift guard is bidirectional: each enum's compiler-generated
//! `VARIANTS` drives the expected set, so adding a role/param/action/kind/grant
//! to the engine fails these tests until the schema lists it too (and a schema
//! that lists a value the engine lacks fails the same `assert_eq`). They also pin
//! the canonical `$id` to `config::SCHEMA_URL`, so the schema, the docs, the
//! example configs, and what `init`/`install` stamp can never drift apart.
//!
//! Validating that real configs *parse* against the schema is the separate,
//! Python-based `just schema-check` (CI job), which a Rust JSON-Schema validator
//! would only provide at the cost of a heavy dependency tree.

use std::collections::BTreeSet;
use std::path::PathBuf;

use allowlister::domain::{Action, Grant, MatchKind, ParamKey, Role};
use serde_json::Value;
use strum::VariantArray;

/// The canonical, publicly hosted location of the schema. Editors and tooling
/// reference this exact string; the example configs embed it as `"$schema"`, and
/// `init`/`install` stamp it onto the configs they write. Tied to the crate
/// constant so the schema file's `$id`, the docs, and what the binary writes can
/// never drift apart.
const SCHEMA_ID: &str = allowlister::config::SCHEMA_URL;

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

/// The wire strings of every variant of a config-vocabulary enum, built from the
/// compiler-generated `VARIANTS`. Because `VARIANTS` grows automatically when a
/// variant is added, an `assert_eq!` against this set fails the moment the engine
/// gains a value the schema has not yet been taught — exactly the drift we guard.
fn engine_set<T: VariantArray + Copy>(wire: impl Fn(T) -> &'static str) -> BTreeSet<String> {
    T::VARIANTS.iter().map(|&v| wire(v).to_string()).collect()
}

#[test]
fn role_enum_matches_the_engine_vocabulary() {
    let schema_roles = enum_values(&schema(), "/$defs/role");
    let engine_roles = engine_set(Role::as_str);
    assert_eq!(
        schema_roles, engine_roles,
        "the schema's role enum must list exactly the engine's roles"
    );
    // Every listed role round-trips through the parser the loader uses.
    for role in &engine_roles {
        assert_eq!(Role::parse(role).map(Role::as_str), Some(role.as_str()));
    }
    assert!(Role::parse("not_a_role").is_none());
}

#[test]
fn param_keys_match_the_engine_vocabulary() {
    // The `params` object's named properties are the canonical parameter keys.
    let schema_params = schema()["$defs"]["params"]["properties"]
        .as_object()
        .expect("params has a properties object")
        .keys()
        .cloned()
        .collect::<BTreeSet<String>>();
    let engine_params = engine_set(ParamKey::as_str);
    assert_eq!(
        schema_params, engine_params,
        "the schema's params keys must be exactly the engine's canonical params"
    );
    for key in &engine_params {
        assert_eq!(
            ParamKey::parse(key).map(ParamKey::as_str),
            Some(key.as_str())
        );
    }
    assert!(ParamKey::parse("nope").is_none());
    // `params` rejects any other key, mirroring the loader.
    assert_eq!(
        schema()["$defs"]["params"]["additionalProperties"],
        Value::Bool(false)
    );
}

#[test]
fn action_kind_and_grant_enums_match_the_engine_vocabulary() {
    let schema = schema();
    let actions = engine_set(Action::as_str);
    let kinds = engine_set(MatchKind::as_str);
    let grants = engine_set(Grant::as_str);
    assert_eq!(enum_values(&schema, "/$defs/action"), actions);
    assert_eq!(enum_values(&schema, "/$defs/kind"), kinds);
    assert_eq!(
        enum_values(&schema, "/$defs/bashRule/properties/grants"),
        grants
    );
    // Each wire string round-trips through the loader's own parser.
    for a in &actions {
        assert_eq!(Action::parse(Some(a)).map(Action::as_str), Ok(a.as_str()));
    }
    for k in &kinds {
        assert_eq!(
            MatchKind::parse(Some(k)).map(MatchKind::as_str),
            Ok(k.as_str())
        );
    }
    for g in &grants {
        assert_eq!(Grant::parse(Some(g)).map(Grant::as_str), Ok(g.as_str()));
    }
}

#[test]
fn schema_declares_dynamic_plugin_config() {
    let schema = schema();
    assert_eq!(
        schema["properties"]["plugins"]["items"]["$ref"].as_str(),
        Some("#/$defs/plugin")
    );
    let plugin = &schema["$defs"]["plugin"];
    assert_eq!(plugin["required"][0].as_str(), Some("command"));
    assert_eq!(
        plugin["properties"]["command"]["minItems"].as_u64(),
        Some(1)
    );
    assert_eq!(
        plugin["properties"]["timeout_ms"]["default"].as_u64(),
        Some(2_000)
    );
    assert_eq!(plugin["additionalProperties"], Value::Bool(false));
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
