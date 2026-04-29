//! CDP utility functions

pub fn extract_string_value(res: &serde_json::Value) -> Option<&str> {
    res.get("value")?.as_str()
}
