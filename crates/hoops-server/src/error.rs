use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub retry_after_secs: Option<u64>,
}

impl ApiError {
    pub fn bad_request(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: msg.into(),
            retry_after_secs: None,
        }
    }

    pub fn forbidden(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code,
            message: msg.into(),
            retry_after_secs: None,
        }
    }

    pub fn not_found(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code,
            message: msg.into(),
            retry_after_secs: None,
        }
    }

    pub fn conflict(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            message: msg.into(),
            retry_after_secs: None,
        }
    }

    pub fn gone(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::GONE,
            code,
            message: msg.into(),
            retry_after_secs: None,
        }
    }

    pub fn too_many(code: &'static str, msg: impl Into<String>, retry_after_secs: u64) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code,
            message: msg.into(),
            retry_after_secs: Some(retry_after_secs),
        }
    }

    pub fn service_unavailable(
        code: &'static str,
        msg: impl Into<String>,
        retry_after_secs: Option<u64>,
    ) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code,
            message: msg.into(),
            retry_after_secs,
        }
    }

    pub fn insufficient_storage(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INSUFFICIENT_STORAGE,
            code,
            message: msg.into(),
            retry_after_secs: None,
        }
    }

    pub fn range_not_satisfiable(msg: impl Into<String>, retry_after_secs: u64) -> Self {
        Self {
            status: StatusCode::RANGE_NOT_SATISFIABLE,
            code: "RangeNotSatisfiable",
            message: msg.into(),
            retry_after_secs: Some(retry_after_secs),
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "InternalServerError",
            message: msg.into(),
            retry_after_secs: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut resp = (
            self.status,
            Json(json!({
                "error": {
                    "code": self.code,
                    "message": self.message,
                }
            })),
        )
            .into_response();
        if let Some(secs) = self.retry_after_secs {
            if let Ok(val) = HeaderValue::from_str(&secs.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, val);
            }
        }
        resp
    }
}
