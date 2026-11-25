//! aba-kit: the pieces a small Rust web app behind a sign-on proxy needs, taken out of an
//! existing app's server so that the next app does not write them again.
//!
//! | Module | What it gives an app |
//! |---|---|
//! | [`identity`] | [`AbaUser`], the user the sign-on proxy names in `X-Aba-User` |
//! | [`http`] | the security headers, the guard against cross-site writes, `/healthz` and `/api/version` |
//! | [`theme`] | `aba-theme.css` at `/aba/aba-theme.css` and the page shell for maud |
//! | [`telemetry`] | the `tracing` setup |
//! | [`sqlite`] | open a database with the house pragmas and migrate it by `PRAGMA user_version` |
//!
//! The README shows each piece in an app; `examples/hello.rs` is all of them in one binary.
//! The rule for this crate: the API stays small, and every change is checked against the apps
//! that use it.
//!
//! The six texts aba-kit writes itself (three in the page shell, three JSON error messages) are
//! German unless the app sets a [`Language`].

pub mod http;
pub mod identity;
mod language;
pub mod sqlite;
pub mod telemetry;
pub mod theme;

pub use identity::{AbaUser, Identity};
pub use language::Language;

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// The JSON body of an error this crate answers with: `code`, `message` and `details`, the
/// shape of the apps' own API errors, so a client reads every error of an app the same way.
#[derive(serde::Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    details: [&'static str; 0],
}

fn error_response(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    let body = ErrorBody {
        code,
        message,
        details: [],
    };
    (status, Json(body)).into_response()
}

#[cfg(test)]
mod test_util {
    use axum::{body::Body, http::Response};
    use http_body_util::BodyExt;

    pub async fn body_string(response: Response<Body>) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    pub fn header_of<'a>(response: &'a Response<Body>, name: &str) -> &'a str {
        response
            .headers()
            .get(name)
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_str()
            .unwrap()
    }
}
