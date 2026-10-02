//! The error every command returns, as the frontend's `ApiError` expects it.

use serde::Serialize;

/// `{ code, message }` with a stable `code` from `API_ERROR_CODES` in `types.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiError {
    /// Stable error code.
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
}

impl ApiError {
    /// An error with `code`.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// An unexpected failure in the app layer.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("internal", message)
    }
}

impl From<dsnap_core::Error> for ApiError {
    fn from(e: dsnap_core::Error) -> Self {
        use dsnap_core::Error as E;
        let code = match &e {
            E::NotFound(_) => "not_found",
            E::ProjectMissing { .. } => "project_missing",
            E::SafetySnapshotFailed { .. } => "safety_snapshot_failed",
            E::Cancelled => "cancelled",
            E::Io { .. } | E::Locked { .. } => "io",
            E::InvalidInput(_) => "invalid_input",
            E::Corrupt(_) | E::BlobMissing(_) => "corrupt",
            E::Db(_) | E::Busy => "db",
            _ => "internal",
        };
        Self::new(code, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_errors_map_to_stable_codes() {
        let c = |e: dsnap_core::Error| ApiError::from(e).code;
        assert_eq!(c(dsnap_core::Error::NotFound("x".into())), "not_found");
        assert_eq!(c(dsnap_core::Error::Cancelled), "cancelled");
        assert_eq!(c(dsnap_core::Error::Busy), "db");
        assert_eq!(
            c(dsnap_core::Error::InvalidInput("x".into())),
            "invalid_input"
        );
        assert_eq!(
            c(dsnap_core::Error::SafetySnapshotFailed {
                source: Box::new(dsnap_core::Error::Cancelled)
            }),
            "safety_snapshot_failed"
        );
    }

    #[test]
    fn serializes_as_code_and_message() {
        let json = serde_json::to_string(&ApiError::new("io", "disk full")).unwrap();
        assert_eq!(json, r#"{"code":"io","message":"disk full"}"#);
    }
}
