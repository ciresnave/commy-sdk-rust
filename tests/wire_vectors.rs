//! Conformance: the golden vectors of the pinned `commy-wire` run through the SDK's own message types.
//!
//! Every control message in `vectors/control_messages.json` that the SDK carries must parse as the
//! SDK's `ClientMessage` or `ServerMessage` and serialize back to an equal JSON value. The messages
//! the SDK does not carry as wire messages are listed in `NOT_CARRIED`, so a new wire message that the
//! SDK forgets fails this test instead of passing silently. The vectors are read from the exact
//! `commy-wire` package Cargo resolved (found with `cargo metadata`), so they cannot drift from the pin.

use commy_sdk_rust::{ClientMessage, ServerMessage};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

/// Wire message types the SDK deliberately does not carry as such, with the reason. Empty since the SDK
/// moved to the commy-wire `Authenticate` / `AuthenticationResult` (0.4.0).
const NOT_CARRIED: &[(&str, &str)] = &[];

/// Invalid wire vectors the SDK accepts on purpose, with the reason. Shrink this.
const TOLERATED_INVALID: &[(&str, &str)] = &[(
    "legacy error spelling",
    "commy's legacy handlers still send CamelCase error codes (NotFound, Unauthorized, ...); the SDK      reads them so the caller sees the real error instead of a timeout",
)];

fn wire_dir() -> PathBuf {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run cargo metadata");
    assert!(out.status.success(), "cargo metadata failed");
    let meta: Value = serde_json::from_slice(&out.stdout).unwrap();
    let manifest = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "commy-wire")
        .expect("commy-wire in the dependency graph")["manifest_path"]
        .as_str()
        .unwrap()
        .to_string();
    PathBuf::from(manifest).parent().unwrap().to_path_buf()
}

fn load(name: &str) -> Value {
    let path = wire_dir().join("vectors").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

fn carried(json: &Value) -> bool {
    let text = json.to_string();
    let as_client = serde_json::from_str::<ClientMessage>(&text)
        .ok()
        .and_then(|m| serde_json::to_value(m).ok());
    let as_server = serde_json::from_str::<ServerMessage>(&text)
        .ok()
        .and_then(|m| serde_json::to_value(m).ok());
    [as_client, as_server]
        .into_iter()
        .flatten()
        .any(|back| back == *json)
}

#[test]
fn every_valid_wire_control_message_round_trips_through_the_sdk_types() {
    let v = load("control_messages.json");
    let mut missing = Vec::new();
    for case in v["valid"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let json = &case["json"];
        let kind = json["type"].as_str().unwrap();
        let expected_gap = NOT_CARRIED.iter().any(|(k, _)| *k == kind);
        match (carried(json), expected_gap) {
            (true, false) | (false, true) => {}
            (false, false) => missing.push(format!("{kind} ({name})")),
            (true, true) => {
                panic!("{kind} ({name}) is now carried by the SDK: remove it from NOT_CARRIED")
            }
        }
    }
    assert!(
        missing.is_empty(),
        "wire messages the SDK does not parse and re-serialize equal: {missing:?}"
    );
}

#[test]
fn invalid_wire_control_messages_are_refused_by_the_sdk_too() {
    let v = load("control_messages.json");
    let mut accepted = Vec::new();
    for case in v["invalid"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let json = &case["json"];
        let kind = json["type"].as_str().unwrap_or("");
        if NOT_CARRIED.iter().any(|(k, _)| *k == kind) {
            continue;
        }
        if carried_loosely(json) && !TOLERATED_INVALID.iter().any(|(n, _)| *n == name) {
            accepted.push(name.to_string());
        }
    }
    assert!(
        accepted.is_empty(),
        "invalid wire messages the SDK accepts: {accepted:?}"
    );
}

/// True when the SDK parses the message at all (as either direction).
fn carried_loosely(json: &Value) -> bool {
    let text = json.to_string();
    serde_json::from_str::<ClientMessage>(&text).is_ok()
        || serde_json::from_str::<ServerMessage>(&text).is_ok()
}

#[test]
fn the_documented_tolerances_are_real_invalid_vectors() {
    let v = load("control_messages.json");
    let names: Vec<&str> = v["invalid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for (name, why) in TOLERATED_INVALID {
        assert!(
            names.contains(name),
            "{name} is not an invalid vector ({why})"
        );
    }
}

#[test]
fn the_documented_gaps_are_real_wire_message_types() {
    let v = load("control_messages.json");
    let types: Vec<&str> = v["valid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["json"]["type"].as_str().unwrap())
        .collect();
    for (kind, why) in NOT_CARRIED {
        assert!(types.contains(kind), "{kind} is not a wire type ({why})");
    }
}

#[test]
fn every_wire_error_code_spelling_is_read() {
    let v = load("error_codes.json");
    for code in v["codes"].as_array().unwrap() {
        let json = format!(
            r#"{{"type":"Error","data":{{"code":{code},"message":"m"}}}}"#,
            code = code
        );
        let m: ServerMessage = serde_json::from_str(&json).unwrap();
        assert!(
            !matches!(
                m,
                ServerMessage::Error {
                    code: commy_sdk_rust::message::ErrorCode::Unknown,
                    ..
                }
            ),
            "{code} read as Unknown"
        );
    }
}
