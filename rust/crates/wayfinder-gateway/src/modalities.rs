//! Fail-closed modality boundary for the gateway.
//!
//! The current gateway ships text execution only. This module makes that
//! boundary explicit so an embeddings, image, audio, or batch-shaped request
//! cannot be silently forwarded to a text provider while those adapters are
//! still being qualified. Each surface will be enabled independently once its
//! provider contract, bounds, accounting, and parity fixtures land.

use serde_json::{Map, Value};

/// Version of the normalized modality contract.
pub const MODALITY_CONTRACT_VERSION: &str = "wf-modalities-v1";

const MAX_SCAN_DEPTH: usize = 64;

/// Validate a Chat Completions body against the currently shipped text-only
/// surface. `None` means the body contains no known unsupported modality.
pub(crate) fn unsupported_text_surface(body: &Map<String, Value>) -> Option<String> {
    for field in [
        "input",
        "images",
        "image",
        "image_url",
        "audio",
        "input_audio",
        "audio_url",
        "modalities",
        "audio_config",
        "voice",
        "format",
        "batch",
        "batch_id",
        "file_id",
        "input_file",
        "files",
    ] {
        if body.contains_key(field) {
            return Some(format!(
                "unsupported modality field '{field}'; this gateway build exposes text execution only"
            ));
        }
    }
    body.get("messages")
        .and_then(|messages| find_nested_modality(messages, 0))
}

fn find_nested_modality(value: &Value, depth: usize) -> Option<String> {
    if depth >= MAX_SCAN_DEPTH {
        return Some(format!(
            "request nesting exceeds the modality validation depth of {MAX_SCAN_DEPTH}"
        ));
    }
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_nested_modality(value, depth.saturating_add(1))),
        Value::Object(object) => {
            for field in [
                "image_url",
                "input_image",
                "image",
                "input_audio",
                "audio",
                "audio_url",
                "input_file",
                "file",
                "file_id",
            ] {
                if object.contains_key(field) {
                    return Some(format!(
                        "unsupported modality field '{field}'; this gateway build exposes text execution only"
                    ));
                }
            }
            object
                .values()
                .find_map(|value| find_nested_modality(value, depth.saturating_add(1)))
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn known_surfaces_fail_closed_before_delivery() {
        for value in [
            json!({"input": "hello"}),
            json!({"messages": [{"role": "user", "content": [{"type": "image_url", "image_url": {"url": "data:"}}]}]}),
            json!({"messages": [{"role": "user", "content": [{"type": "input_audio", "input_audio": {"data": "..."}}]}]}),
            json!({"batch": {"id": "batch-1"}}),
        ] {
            assert!(unsupported_text_surface(&object(value)).is_some());
        }
    }

    #[test]
    fn ordinary_text_body_is_accepted() {
        let body = object(json!({
            "messages": [{"role": "user", "content": "hello"}],
            "stream": false
        }));
        assert_eq!(unsupported_text_surface(&body), None);
    }

    #[test]
    fn nested_validation_has_a_depth_bound() {
        let mut value = json!("text");
        for _ in 0..=MAX_SCAN_DEPTH {
            value = json!([value]);
        }
        let body = object(json!({"messages": value}));
        assert!(
            unsupported_text_surface(&body)
                .is_some_and(|message| message.contains("modality validation depth"))
        );
    }
}
