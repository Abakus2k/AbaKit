//! What every app's router is wrapped in, and the two routes every app has.
//!
//! ```
//! use axum::{Router, middleware, routing::get};
//! use aba_kit::http::{self, VersionInfo};
//!
//! let version = VersionInfo::new(
//!     env!("CARGO_PKG_NAME"),
//!     env!("CARGO_PKG_VERSION"),
//!     option_env!("GIT_SHA"),
//! );
//! let app: Router = Router::new()
//!     .route("/", get(|| async { "hello" }))
//!     .merge(http::health_router(version))
//!     .layer(middleware::from_fn(http::reject_cross_site_writes))
//!     .layer(middleware::from_fn(http::security_headers));
//! ```
//!
//! - [`security_headers`]: the Content-Security-Policy and four more headers on every response.
//! - [`reject_cross_site_writes`]: the CSRF guard. It is a rule, not an option, for every app
//!   that takes its user from [`crate::identity`]: the identity comes from the network, so
//!   without the guard any web page the user opens could change the app's state. Its 403
//!   message is German; [`reject_cross_site_writes_in`] is the same guard with the message in
//!   another [`Language`].
//! - [`health_router`]: `GET /healthz` and `GET /api/version`.
//! - [`healthcheck`]: the probe behind an app's `--healthcheck` flag (an image's HEALTHCHECK).

use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::Response,
    routing::get,
};
use serde::Serialize;

use crate::Language;

/// Content-Security-Policy of every response. Everything stays on the app's own origin: no
/// CDN, no external font, no analytics, no inline script, no inline style.
/// `'wasm-unsafe-eval'` lets a page compile a Leptos client's WebAssembly; it allows no
/// JavaScript `eval`.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
style-src 'self'; img-src 'self' data:; connect-src 'self'; font-src 'self'; base-uri 'self'; \
form-action 'self'; frame-ancestors 'none'";

/// Where [`health_router`] answers `ok`.
pub const HEALTH_PATH: &str = "/healthz";

/// Where [`health_router`] answers the [`VersionInfo`].
pub const VERSION_PATH: &str = "/api/version";

const NO_STORE: [(HeaderName, &str); 1] = [(header::CACHE_CONTROL, "no-store")];

/// Middleware (`axum::middleware::from_fn`): the security headers on every response, whatever
/// its status. Put it outermost, so that it also covers what other layers answer.
pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    response
}

/// The host (and port) an `Origin` header names: `https://app.example.org` ->
/// `app.example.org`. None for `null` and anything that is not `scheme://host[:port]`.
fn origin_host(origin: &str) -> Option<&str> {
    let (scheme, rest) = origin.split_once("://")?;
    let host = rest.strip_suffix('/').unwrap_or(rest);
    let ok = !scheme.is_empty()
        && !host.is_empty()
        && !host.contains('/')
        && !host.contains('@')
        && (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"));
    ok.then_some(host)
}

/// `host` with the scheme's default port dropped, lower-cased: `Example.org:443` and
/// `example.org` compare equal.
fn normalize_host(host: &str, default_port: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    host.strip_suffix(&format!(":{default_port}"))
        .map(str::to_owned)
        .unwrap_or(host)
}

/// Whether the `Origin` of a request names the host the request was sent to (`Host`, or
/// `X-Forwarded-Host` from a proxy that rewrites `Host`; a browser cannot set that one on a
/// cross-site request without a preflight).
fn same_origin(headers: &HeaderMap, origin: &str) -> bool {
    let Some(origin_host) = origin_host(origin.trim()) else {
        return false;
    };
    let default_port = if origin
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("https")
    {
        "443"
    } else {
        "80"
    };
    let want = normalize_host(origin_host, default_port);
    [header::HOST.as_str(), "x-forwarded-host"]
        .iter()
        .filter_map(|name| headers.get(*name).and_then(|v| v.to_str().ok()))
        .flat_map(|v| v.split(','))
        .any(|h| normalize_host(h, default_port) == want)
}

/// Whether a write comes from elsewhere, by the rule of [`reject_cross_site_writes`].
fn comes_from_elsewhere(headers: &HeaderMap) -> bool {
    let mut sites = headers.get_all("sec-fetch-site").iter();
    match (sites.next(), sites.next()) {
        // The browser says where the request comes from, and that decides.
        (Some(site), None) => !site
            .to_str()
            .is_ok_and(|v| v.eq_ignore_ascii_case("same-origin") || v.eq_ignore_ascii_case("none")),
        // Two values: no browser sends that.
        (Some(_), Some(_)) => true,
        // No word of the browser's: the `Origin`, when there is one, must be this host.
        (None, _) => headers
            .get(header::ORIGIN)
            .is_some_and(|v| v.to_str().map_or(true, |o| !same_origin(headers, o))),
    }
}

/// Middleware (`axum::middleware::from_fn`): the guard against cross-site writes.
///
/// An app has no login of its own: the sign-on proxy names the user on every request the
/// browser sends, whichever page started it. So a page from another site must not make the
/// browser change state here. GET, HEAD and OPTIONS always pass (the
/// other half of the rule is the app's: a GET never changes state). Every other method is a
/// write, and what the browser itself asserts decides first:
///
/// | `Sec-Fetch-Site` | `Origin` | write |
/// |---|---|---|
/// | `same-origin` or `none` | anything, `null` and none included | passes |
/// | `cross-site` or `same-site` | anything | 403 |
/// | another value, or the header twice | anything | 403 |
/// | not sent | not sent | passes (curl, the health check, scripts) |
/// | not sent | this host (`Host` or `X-Forwarded-Host`, same port) | passes |
/// | not sent | another host, another port, `null`, or not readable | 403 |
///
/// Why `Sec-Fetch-Site` wins over `Origin`: every page is served with
/// `Referrer-Policy: no-referrer` ([`security_headers`]), and under that policy the Fetch
/// standard sends `Origin: null` with a POST, also with a form POST to the page's own origin.
/// So `Origin` cannot tell the app's own form from a foreign one, while `Sec-Fetch-Site` can:
/// it is a forbidden header name (`Sec-`), so no page script can set or change it, and the
/// browser computes it from the origin that started the request and every redirect on the way.
///
/// What this lets through that the old rule (an `Origin` other than this host refused, whatever
/// `Sec-Fetch-Site` said) refused, and why that is safe:
/// - `same-origin`: a document of exactly this origin (scheme, host and port) started the
///   request, and no hop of a redirect went through another site (one would make it
///   `cross-site`). That is the app's own form or script. Another app on the same host but
///   another port is `same-site` and stays refused, which matters on a development machine,
///   where several apps listen on 127.0.0.1. The pages cannot be framed (`frame-ancestors 'none'`,
///   `X-Frame-Options: DENY`), and a sandboxed or `data:` document has an opaque origin, which
///   makes its requests `cross-site`.
/// - `none`: the person started it in the browser (a typed address, a bookmark, a reload); no
///   other page took part.
/// - With either value and a foreign `Origin`: no browser sends that pair. Only a program that
///   writes its headers itself does, and such a program could as well send neither header,
///   which always passed: it is no way in for another site's page.
///
/// What stays refused: every write a browser marks `cross-site` or `same-site`; a value no
/// browser sends (fail closed); and, from a client without `Sec-Fetch-Site` (browsers before
/// Fetch Metadata, Safari before 16.4), a foreign `Origin` and `Origin: null`. Such an old
/// browser sends `Origin: null` with this app's own forms as well (`no-referrer`), so it cannot
/// write here: closed on purpose, because a sandboxed page sends the same `null`. The
/// referrer policy stays `no-referrer`: changing it would only help those old browsers, and it
/// would send every page's address (with its ids and query string) along as `Referer`.
///
/// The 403 carries a German message; [`reject_cross_site_writes_in`] writes it in another
/// language.
pub async fn reject_cross_site_writes(request: Request, next: Next) -> Response {
    reject_cross_site_writes_in(State(Language::default()), request, next).await
}

/// [`reject_cross_site_writes`] with the message of its 403 (`{"code": "forbidden", ...}`) in
/// `language`. It is the same guard in every other respect, and an app takes the language as the
/// middleware's state:
///
/// ```
/// use aba_kit::{Language, http};
/// use axum::{Router, middleware};
///
/// let app: Router = Router::new().layer(middleware::from_fn_with_state(
///     Language::English,
///     http::reject_cross_site_writes_in,
/// ));
/// ```
pub async fn reject_cross_site_writes_in(
    State(language): State<Language>,
    request: Request,
    next: Next,
) -> Response {
    let write = !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    if write && comes_from_elsewhere(request.headers()) {
        return crate::error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            language.cross_site_write(),
        );
    }
    next.run(request).await
}

/// Body of `GET /api/version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VersionInfo {
    /// The app's package name.
    pub name: &'static str,
    /// The app's package version.
    pub version: &'static str,
    /// The commit the build came from, if known.
    pub git_sha: Option<String>,
}

impl VersionInfo {
    /// The app's version, called as `VersionInfo::new(env!("CARGO_PKG_NAME"),
    /// env!("CARGO_PKG_VERSION"), option_env!("GIT_SHA"))`. The commit is the `GIT_SHA` of the
    /// process environment, else the one baked in at build time (`GIT_SHA=... cargo build`).
    pub fn new(
        name: &'static str,
        version: &'static str,
        built_git_sha: Option<&'static str>,
    ) -> Self {
        VersionInfo {
            name,
            version,
            git_sha: pick_git_sha(std::env::var("GIT_SHA").ok().as_deref(), built_git_sha),
        }
    }
}

/// The runtime value wins over the built-in one; an empty value counts as unset.
fn pick_git_sha(runtime: Option<&str>, built: Option<&str>) -> Option<String> {
    [runtime, built]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|sha| !sha.is_empty())
        .map(str::to_owned)
}

/// `GET /healthz` (the text `ok`) and `GET /api/version` (the [`VersionInfo`] as JSON), both
/// `Cache-Control: no-store`. Merge it outside an app's request-log layer: a container health
/// check calls `/healthz` on a timer (every 30 seconds by default).
pub fn health_router<S>(version: VersionInfo) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(HEALTH_PATH, get(|| async { (NO_STORE, "ok") }))
        .route(
            VERSION_PATH,
            get(move || {
                let version = version.clone();
                async move { (NO_STORE, Json(version)) }
            }),
        )
}

/// Address the health check connects to: the bind address, or loopback if that is a wildcard.
fn probe_addr(addr: SocketAddr) -> SocketAddr {
    let ip = match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, addr.port())
}

/// The container health check behind an app's `--healthcheck` flag. A minimal runtime image has
/// no shell and no curl, so the binary asks itself: true if `GET /healthz` on `addr` (the address
/// the server binds; a wildcard is asked on loopback) answers 200 within two seconds.
pub fn healthcheck(addr: SocketAddr) -> bool {
    let timeout = Duration::from_secs(2);
    let Ok(mut stream) = TcpStream::connect_timeout(&probe_addr(addr), timeout) else {
        return false;
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return false;
    }
    let request = b"GET /healthz HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    if stream.write_all(request).is_err() {
        return false;
    }
    // "HTTP/1.1 200": the first 12 bytes of the status line.
    let mut status_line = [0u8; 12];
    stream.read_exact(&mut status_line).is_ok()
        && status_line.starts_with(b"HTTP/1.")
        && &status_line[8..] == b" 200"
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::Request,
        middleware,
        routing::{post, put},
    };
    use tower::ServiceExt;

    use super::*;
    use crate::test_util::{body_string, header_of};

    fn version() -> VersionInfo {
        VersionInfo {
            name: "test-app",
            version: "1.2.3",
            git_sha: Some("0123abc".to_owned()),
        }
    }

    /// An app as the module docs build it: a page, an API with writes, a form target.
    fn app() -> Router {
        Router::new()
            .route("/", get(|| async { "page" }))
            .route(
                "/api/things",
                get(|| async { "list" })
                    .post(|| async { "made" })
                    .patch(|| async { "changed" })
                    .delete(|| async { "gone" }),
            )
            .route("/api/things/1", put(|| async { "replaced" }))
            .route("/form", post(|| async { "sent" }))
            .merge(health_router(version()))
            .layer(middleware::from_fn(reject_cross_site_writes))
            .layer(middleware::from_fn(security_headers))
    }

    async fn send_to(app: Router, method: Method, uri: &str, headers: &[(&str, &str)]) -> Response {
        let mut request = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        app.oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn send(method: Method, uri: &str, headers: &[(&str, &str)]) -> Response {
        send_to(app(), method, uri, headers).await
    }

    /// The same app with the guard that takes its language as state.
    fn app_in(language: Language) -> Router {
        Router::new()
            .route(
                "/api/things",
                get(|| async { "list" }).post(|| async { "made" }),
            )
            .layer(middleware::from_fn_with_state(
                language,
                reject_cross_site_writes_in,
            ))
            .layer(middleware::from_fn(security_headers))
    }

    async fn message_of(response: Response) -> serde_json::Value {
        assert_eq!(header_of(&response, "content-type"), "application/json");
        serde_json::from_str(&body_string(response).await).unwrap()
    }

    async fn status(method: Method, uri: &str, headers: &[(&str, &str)]) -> StatusCode {
        send(method, uri, headers).await.status()
    }

    fn assert_security_headers(response: &Response) {
        assert_eq!(
            header_of(response, "content-security-policy"),
            CONTENT_SECURITY_POLICY
        );
        assert_eq!(header_of(response, "x-content-type-options"), "nosniff");
        assert_eq!(header_of(response, "referrer-policy"), "no-referrer");
        assert_eq!(header_of(response, "x-robots-tag"), "noindex, nofollow");
        assert_eq!(header_of(response, "x-frame-options"), "DENY");
    }

    #[test]
    fn csp_is_strict() {
        assert!(CONTENT_SECURITY_POLICY.starts_with("default-src 'self'"));
        assert!(CONTENT_SECURITY_POLICY.contains("frame-ancestors 'none'"));
        // The only exception: compiling a client's WebAssembly. No eval, no inline script.
        assert_eq!(
            CONTENT_SECURITY_POLICY.matches("unsafe-").count(),
            1,
            "{CONTENT_SECURITY_POLICY}"
        );
        assert!(CONTENT_SECURITY_POLICY.contains("script-src 'self' 'wasm-unsafe-eval'"));
        assert!(!CONTENT_SECURITY_POLICY.contains("'unsafe-inline'"));
        assert!(!CONTENT_SECURITY_POLICY.contains("'unsafe-eval'"));
        assert!(!CONTENT_SECURITY_POLICY.contains("http"));
    }

    #[tokio::test]
    async fn every_response_carries_the_security_headers() {
        for (method, uri, want) in [
            (Method::GET, "/", StatusCode::OK),
            (Method::GET, "/healthz", StatusCode::OK),
            (Method::GET, "/api/version", StatusCode::OK),
            (Method::GET, "/nope", StatusCode::NOT_FOUND),
            (Method::DELETE, "/", StatusCode::METHOD_NOT_ALLOWED),
        ] {
            let response = send(method, uri, &[]).await;
            assert_eq!(response.status(), want, "{uri}");
            assert_security_headers(&response);
        }
        // Also what the guard itself answers.
        let refused = send(Method::POST, "/form", &[("sec-fetch-site", "cross-site")]).await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert_security_headers(&refused);
    }

    /// A page from another site cannot make the browser change state: a form POST needs no
    /// CORS preflight, so `Sec-Fetch-Site` decides.
    #[tokio::test]
    async fn cross_site_writes_are_refused() {
        let writes = [
            (Method::POST, "/api/things"),
            (Method::PATCH, "/api/things"),
            (Method::DELETE, "/api/things"),
            (Method::PUT, "/api/things/1"),
            // Not only under /api: a maud app's form targets are guarded too.
            (Method::POST, "/form"),
        ];
        for (method, uri) in &writes {
            for site in ["cross-site", "same-site", "Cross-Site"] {
                assert_eq!(
                    status(method.clone(), uri, &[("sec-fetch-site", site)]).await,
                    StatusCode::FORBIDDEN,
                    "{method} {uri} {site}"
                );
            }
            // The app itself, a typed address and tools without the header may write.
            for site in ["same-origin", "none"] {
                assert_eq!(
                    status(method.clone(), uri, &[("sec-fetch-site", site)]).await,
                    StatusCode::OK,
                    "{method} {uri} {site}"
                );
            }
            assert_eq!(
                status(method.clone(), uri, &[]).await,
                StatusCode::OK,
                "{uri}"
            );
        }
        // Reads stay open.
        for method in [Method::GET, Method::HEAD] {
            assert_eq!(
                status(method, "/api/things", &[("sec-fetch-site", "cross-site")]).await,
                StatusCode::OK
            );
        }
    }

    #[tokio::test]
    async fn the_refusal_is_a_json_error() {
        let response = send(
            Method::POST,
            "/api/things",
            &[("sec-fetch-site", "cross-site")],
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(header_of(&response, "content-type"), "application/json");
        let json: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
        assert_eq!(json["code"], "forbidden");
        assert_eq!(json["message"], "Änderungen nur aus der App selbst");
        assert_eq!(json["details"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn the_refusal_speaks_the_language_it_is_given() {
        /// A write to the API through the default guard (`None`) or the one with a language.
        async fn post(language: Option<Language>, headers: &[(&str, &str)]) -> Response {
            let app = language.map_or_else(app, app_in);
            send_to(app, Method::POST, "/api/things", headers).await
        }
        let cross_site: &[(&str, &str)] = &[("sec-fetch-site", "cross-site")];
        let foreign_origin: &[(&str, &str)] = &[
            ("host", "app.example.org"),
            ("origin", "https://evil.example"),
        ];

        for (language, message) in [
            (None, "Änderungen nur aus der App selbst"),
            (Some(Language::German), "Änderungen nur aus der App selbst"),
            (Some(Language::English), "Changes only from the app itself"),
        ] {
            for headers in [cross_site, foreign_origin] {
                let response = post(language, headers).await;
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{language:?}");
                assert_security_headers(&response);
                assert_eq!(
                    message_of(response).await,
                    serde_json::json!({"code": "forbidden", "message": message, "details": []}),
                    "{language:?}"
                );
            }
            // The guard lets through what it always let through, whatever the language.
            for headers in [
                &[("sec-fetch-site", "same-origin")][..],
                &[("sec-fetch-site", "none")],
                &[],
                &[
                    ("host", "app.example.org"),
                    ("origin", "https://app.example.org"),
                ],
            ] {
                let response = post(language, headers).await;
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "{language:?} {headers:?}"
                );
            }
        }

        // Reads stay open in English too.
        let read = send_to(
            app_in(Language::English),
            Method::GET,
            "/api/things",
            &[("sec-fetch-site", "cross-site")],
        )
        .await;
        assert_eq!(read.status(), StatusCode::OK);
    }

    /// Older browsers send no `Sec-Fetch-Site`, but an `Origin`: it must be this host.
    #[tokio::test]
    async fn a_foreign_origin_is_refused() {
        let with_origin = |origin: &'static str, host: &'static str| async move {
            status(
                Method::POST,
                "/api/things",
                &[("host", host), ("origin", origin)],
            )
            .await
        };
        for origin in [
            "https://evil.example",
            "null",
            "https://other.example.org.evil.example",
            "https://evil.example/other.example.org",
            "https://other.example.org@evil.example",
            "ftp://other.example.org",
        ] {
            assert_eq!(
                with_origin(origin, "other.example.org").await,
                StatusCode::FORBIDDEN,
                "{origin}"
            );
        }
        for (origin, host) in [
            ("https://other.example.org", "other.example.org"),
            ("http://127.0.0.1:8080", "127.0.0.1:8080"),
            ("https://Other.Example.org:443", "other.example.org"),
            ("http://app.example.org:80/", "app.example.org"),
        ] {
            assert_eq!(with_origin(origin, host).await, StatusCode::OK, "{origin}");
        }
        // Another port is another origin.
        assert_eq!(
            with_origin("http://127.0.0.1:8081", "127.0.0.1:8080").await,
            StatusCode::FORBIDDEN
        );
        // A proxy that rewrites Host names the public host in X-Forwarded-Host.
        assert_eq!(
            status(
                Method::POST,
                "/api/things",
                &[
                    ("host", "127.0.0.1:8080"),
                    ("x-forwarded-host", "other.example.org"),
                    ("origin", "https://other.example.org"),
                ],
            )
            .await,
            StatusCode::OK
        );
        // With Sec-Fetch-Site the browser's word decides, not the Origin: the table test below.
    }

    /// Every row of the guard's table (the doc of `reject_cross_site_writes`), for every method
    /// that writes and every one that does not.
    #[tokio::test]
    async fn the_guard_follows_its_table() {
        const PASS: StatusCode = StatusCode::OK;
        const REFUSED: StatusCode = StatusCode::FORBIDDEN;
        const HOST: &str = "app.example.org";
        // What Sec-Fetch-Site says (None: not sent; two entries: the header twice).
        let sites: [(&str, &[&str]); 9] = [
            ("not sent", &[]),
            ("same-origin", &["same-origin"]),
            ("none", &["none"]),
            ("cross-site", &["cross-site"]),
            ("same-site", &["same-site"]),
            ("unknown value", &["same-orign"]),
            ("empty", &[""]),
            ("twice", &["same-origin", "same-origin"]),
            ("twice, mixed", &["same-origin", "cross-site"]),
        ];
        // What Origin says.
        let origins: [(&str, Option<&str>); 6] = [
            ("not sent", None),
            ("this host", Some("https://app.example.org")),
            ("another host", Some("https://evil.example")),
            (
                "another app of the domain",
                Some("https://other.example.org"),
            ),
            (
                "this host, another port",
                Some("https://app.example.org:8443"),
            ),
            ("null", Some("null")),
        ];
        let want = |site: &str, origin: &str| match site {
            "same-origin" | "none" => PASS,
            "not sent" => match origin {
                "not sent" | "this host" => PASS,
                _ => REFUSED,
            },
            _ => REFUSED,
        };

        let mut rows = 0;
        for (site_name, site_values) in &sites {
            for (origin_name, origin) in &origins {
                let mut headers = vec![("host", HOST)];
                headers.extend(site_values.iter().map(|v| ("sec-fetch-site", *v)));
                headers.extend(origin.map(|o| ("origin", o)));
                for (method, uri) in [
                    (Method::POST, "/api/things"),
                    (Method::POST, "/form"),
                    (Method::PATCH, "/api/things"),
                    (Method::DELETE, "/api/things"),
                    (Method::PUT, "/api/things/1"),
                ] {
                    assert_eq!(
                        status(method.clone(), uri, &headers).await,
                        want(site_name, origin_name),
                        "{method} {uri}, Sec-Fetch-Site {site_name}, Origin {origin_name}"
                    );
                }
                // A read always passes, whatever the headers say.
                for method in [Method::GET, Method::HEAD] {
                    assert_eq!(
                        status(method.clone(), "/api/things", &headers).await,
                        PASS,
                        "{method}, Sec-Fetch-Site {site_name}, Origin {origin_name}"
                    );
                }
                // OPTIONS is no write either: the guard lets it on to the router, which has no
                // OPTIONS route here (405), never the guard's 403.
                let options = send(Method::OPTIONS, "/api/things", &headers).await;
                assert_eq!(
                    options.status(),
                    StatusCode::METHOD_NOT_ALLOWED,
                    "OPTIONS, Sec-Fetch-Site {site_name}, Origin {origin_name}"
                );
                rows += 1;
            }
        }
        assert_eq!(rows, 54);
    }

    /// What a browser sends with a form POST of this app's own page: every page is served with
    /// `Referrer-Policy: no-referrer`, and under that policy the Fetch standard sends
    /// `Origin: null` on a POST, also to the page's own origin. `Sec-Fetch-Site: same-origin`
    /// says where it comes from. A rule that let `Origin` decide refused every such form.
    #[tokio::test]
    async fn a_browsers_own_form_post_passes() {
        let own_form = [
            ("host", "127.0.0.1:8080"),
            ("sec-fetch-site", "same-origin"),
            ("origin", "null"),
            ("content-type", "application/x-www-form-urlencoded"),
        ];
        assert_eq!(
            status(Method::POST, "/form", &own_form).await,
            StatusCode::OK
        );
    }

    #[test]
    fn origins_are_compared_by_host_and_port() {
        assert_eq!(origin_host("https://a.example"), Some("a.example"));
        assert_eq!(
            origin_host("http://a.example:8080/"),
            Some("a.example:8080")
        );
        for bad in ["null", "a.example", "://a.example", "https://", "file:///x"] {
            assert_eq!(origin_host(bad), None, "{bad}");
        }
        assert_eq!(normalize_host(" Example.ORG:443 ", "443"), "example.org");
        assert_eq!(
            normalize_host("example.org:8443", "443"),
            "example.org:8443"
        );
    }

    #[tokio::test]
    async fn healthz_answers_ok_and_is_never_cached() {
        let response = send(Method::GET, "/healthz", &[]).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header_of(&response, "cache-control"), "no-store");
        assert_eq!(body_string(response).await, "ok");
        assert_eq!(
            status(Method::POST, "/healthz", &[]).await,
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[tokio::test]
    async fn version_reports_name_version_and_git_sha() {
        let response = send(Method::GET, "/api/version", &[]).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header_of(&response, "content-type"), "application/json");
        assert_eq!(header_of(&response, "cache-control"), "no-store");
        let json: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"name": "test-app", "version": "1.2.3", "git_sha": "0123abc"})
        );

        let unknown = VersionInfo {
            git_sha: None,
            ..version()
        };
        let response = health_router::<()>(unknown)
            .oneshot(
                Request::builder()
                    .uri("/api/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
        assert!(json["git_sha"].is_null());
    }

    /// An app that answers a wrong method with its own JSON 405 covers these two routes as
    /// well when their router is merged before `method_not_allowed_fallback` (README).
    #[tokio::test]
    async fn an_apps_method_fallback_covers_the_health_routes() {
        let app: Router = Router::new()
            .merge(health_router(version()))
            .method_not_allowed_fallback(|| async {
                (StatusCode::METHOD_NOT_ALLOWED, "the app says no")
            });
        for uri in [HEALTH_PATH, VERSION_PATH] {
            let request = Request::builder()
                .method(Method::DELETE)
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{uri}");
            assert_eq!(body_string(response).await, "the app says no", "{uri}");
        }
    }

    #[test]
    fn the_runtime_git_sha_wins_over_the_built_in_one() {
        assert_eq!(
            pick_git_sha(Some("aaa"), Some("bbb")).as_deref(),
            Some("aaa")
        );
        assert_eq!(pick_git_sha(None, Some("bbb")).as_deref(), Some("bbb"));
        assert_eq!(pick_git_sha(Some(" "), Some("bbb")).as_deref(), Some("bbb"));
        assert_eq!(pick_git_sha(Some(""), Some("")), None);
        assert_eq!(pick_git_sha(None, None), None);
        // The app's own name and version go in as they are.
        let info = VersionInfo::new("some-app", "0.3.0", None);
        assert_eq!((info.name, info.version), ("some-app", "0.3.0"));
    }

    #[test]
    fn a_wildcard_bind_address_is_probed_on_loopback() {
        let probe = |addr: &str| probe_addr(addr.parse().unwrap()).to_string();
        assert_eq!(probe("0.0.0.0:8080"), "127.0.0.1:8080");
        assert_eq!(probe("[::]:8080"), "[::1]:8080");
        assert_eq!(probe("127.0.0.1:3000"), "127.0.0.1:3000");
        assert_eq!(probe("192.0.2.5:3000"), "192.0.2.5:3000");
    }

    /// The probe against real sockets: a healthy app, an app without `/healthz`, and nothing.
    #[tokio::test]
    async fn the_healthcheck_asks_a_running_server() {
        async fn serve(router: Router) -> SocketAddr {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, router).await });
            addr
        }
        async fn probe(addr: SocketAddr) -> bool {
            tokio::task::spawn_blocking(move || healthcheck(addr))
                .await
                .unwrap()
        }

        let healthy = serve(app()).await;
        assert!(probe(healthy).await);
        // Asked on the wildcard address, the probe goes to loopback.
        let wildcard = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), healthy.port());
        assert!(probe(wildcard).await);

        let no_health_route = serve(Router::new().route("/", get(|| async { "page" }))).await;
        assert!(!probe(no_health_route).await);

        let closed = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        assert!(!probe(closed).await);
    }
}
