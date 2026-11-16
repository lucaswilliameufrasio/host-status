use serde::Serialize;

#[derive(Serialize)]
pub struct BuildInfo {
    version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dirty: Option<bool>,
}

pub fn current() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION"),
        commit: option_env!("HOST_STATUS_COMMIT"),
        dirty: option_env!("HOST_STATUS_DIRTY").and_then(|value| value.parse().ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_info_serializes_version_and_omits_unknown_values() {
        let info = BuildInfo {
            version: "1.2.3",
            commit: None,
            dirty: None,
        };
        assert_eq!(
            serde_json::to_value(info).unwrap(),
            serde_json::json!({"version": "1.2.3"})
        );
    }

    #[test]
    fn build_info_preserves_commit_and_dirty_state() {
        let info = BuildInfo {
            version: "1.2.3",
            commit: Some("abcdef"),
            dirty: Some(true),
        };
        assert_eq!(
            serde_json::to_value(info).unwrap(),
            serde_json::json!({"version": "1.2.3", "commit": "abcdef", "dirty": true})
        );
    }
}
