//! Who a request comes from. No app stores passwords or a user table: the sign-on proxy in front
//! of the app names the user in request headers, and the app believes them only from that proxy.
//!
//! The contract:
//! - `X-Aba-User`: the stable user id, for example `aba`. 1 to 64 characters out of ASCII
//!   letters, digits and `-`, `_`, `.`, `@`.
//! - `X-Aba-Name`: the display name, percent-encoded UTF-8. Optional; the id stands in for it.
//! - `X-Aba-Apps`: the app ids this person may use, separated by commas (`notes,photos,wiki`),
//!   as the proxy knows them. Optional; [`AbaUser::apps`] is `None` without it, and an app
//!   that shows what a person may use shows nothing then.
//! - All three count only when the TCP peer is one of the trusted addresses: `ABA_TRUSTED_PEERS`,
//!   exact IP addresses separated by commas, by default loopback. From any other peer the
//!   headers are ignored, so a client cannot name itself.
//! - `ABA_DEV_USER` is the user of every request without a trusted header: for local runs and
//!   demo builds. Never set it on the server.
//!
//! An app puts an [`Identity`] into its state and takes [`AbaUser`] (401 without a user) or
//! `Option<AbaUser>` as a handler argument. The server must be started with
//! `into_make_service_with_connect_info::<SocketAddr>()`; without the peer address nobody is
//! trusted.
//!
//! The identity is ambient, so [`crate::http::reject_cross_site_writes`] belongs to every app
//! that uses it.
//!
//! The extractors answer a request without a user with a small JSON error (401, 400) whose
//! message is German. [`Identity::with_language`] writes the two messages in another
//! [`Language`].

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};

use axum::{
    extract::{ConnectInfo, FromRef, FromRequestParts, OptionalFromRequestParts},
    http::{HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use percent_encoding::percent_decode_str;

use crate::Language;

/// The header with the user id. Public because an app that asks another app behind the same
/// proxy for something on a person's behalf sets it on its own request: keep it.
pub const USER_HEADER: &str = "x-aba-user";

/// The header with the display name (percent-encoded UTF-8).
pub const NAME_HEADER: &str = "x-aba-name";

/// The header with the app ids the person may use, separated by commas.
pub const APPS_HEADER: &str = "x-aba-apps";

/// Environment variable: the addresses whose identity headers count.
pub const TRUSTED_PEERS_ENV: &str = "ABA_TRUSTED_PEERS";

/// Environment variable: the user of requests without a trusted header.
pub const DEV_USER_ENV: &str = "ABA_DEV_USER";

/// The user a request acts as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbaUser {
    /// The stable id that keys the user's rows, the same in every app.
    pub id: String,
    /// The name to show. The id when the proxy sent none.
    pub name: String,
    /// The app ids the sign-on proxy says this person may use (`X-Aba-Apps`), in its order.
    /// `None` when the request carried no trusted list: no header, or the dev user. A list
    /// with an entry that is not an app id is dropped whole (`Some` of an empty list), so a
    /// broken header grants nothing.
    pub apps: Option<Vec<String>>,
}

/// The rules for believing the identity headers. Cheap to clone; lives in the app's state.
#[derive(Debug, Clone)]
pub struct Identity(Arc<Rules>);

#[derive(Debug, Clone)]
struct Rules {
    trusted_peers: Vec<IpAddr>,
    dev_user: Option<String>,
    language: Language,
}

impl Default for Identity {
    /// Trusts loopback; no dev user; German error messages.
    fn default() -> Self {
        Identity(Arc::new(Rules {
            trusted_peers: loopback(),
            dev_user: None,
            language: Language::default(),
        }))
    }
}

fn loopback() -> Vec<IpAddr> {
    vec![
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ]
}

/// The rule for a user id: short, and safe in a file name, a URL and a log line.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@'))
}

/// The rule for an app id (the ids the sign-on proxy gives its apps): lower-case letters,
/// digits and `-`, at most 32 characters.
fn valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The app ids out of one `X-Aba-Apps` value. Empty parts are skipped; one part that is not an
/// app id drops the whole list, because a header that does not parse must grant nothing.
fn app_list(raw: &str) -> Vec<String> {
    let mut apps: Vec<String> = Vec::new();
    for part in raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if !valid_app_id(part) {
            tracing::warn!(
                "X-Aba-Apps holds a part that is not an app id; no apps for this request"
            );
            return Vec::new();
        }
        if !apps.iter().any(|app| app == part) {
            apps.push(part.to_owned());
        }
    }
    apps
}

/// The display name out of `X-Aba-Name`, or None when it is not a usable name.
fn display_name(raw: &str) -> Option<String> {
    let decoded = percent_decode_str(raw.trim()).decode_utf8().ok()?;
    let name = decoded.trim();
    let usable =
        !name.is_empty() && name.chars().count() <= 64 && !name.chars().any(char::is_control);
    usable.then(|| name.to_owned())
}

impl Identity {
    /// Trusts exactly `trusted_peers` (none, when empty: every request is then the dev user or
    /// nobody). Fails when `dev_user` is not a valid user id.
    pub fn new(trusted_peers: &[IpAddr], dev_user: Option<&str>) -> Result<Self, String> {
        if let Some(id) = dev_user
            && !valid_id(id)
        {
            return Err(format!(
                "{DEV_USER_ENV} must be a user id (letters, digits, - _ . @; at most 64), got {id:?}"
            ));
        }
        Ok(Identity(Arc::new(Rules {
            trusted_peers: trusted_peers.iter().map(IpAddr::to_canonical).collect(),
            dev_user: dev_user.map(str::to_owned),
            language: Language::default(),
        })))
    }

    /// The same rules with the messages of the two JSON errors the extractors answer with (401
    /// without a user, 400 for a malformed one) written in `language`. Only those two texts
    /// change: who is trusted and who the user is stay as they are. Without this call they
    /// are German.
    #[must_use]
    pub fn with_language(self, language: Language) -> Self {
        let rules = Arc::unwrap_or_clone(self.0);
        Identity(Arc::new(Rules { language, ..rules }))
    }

    /// Reads `ABA_TRUSTED_PEERS` and `ABA_DEV_USER` from the process environment.
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Reads the two settings through `lookup`. An empty value counts as unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| lookup(key).filter(|value| !value.trim().is_empty());
        let trusted_peers = match get(TRUSTED_PEERS_ENV) {
            None => loopback(),
            Some(raw) => {
                let peers = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(|part| {
                        part.parse::<IpAddr>().map_err(|_| {
                            format!(
                                "{TRUSTED_PEERS_ENV} must be exact IP addresses separated by \
                                 commas (no ranges, no host names), got {part:?}"
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if peers.is_empty() {
                    return Err(format!("{TRUSTED_PEERS_ENV} names no address: {raw:?}"));
                }
                peers
            }
        };
        let dev_user = get(DEV_USER_ENV);
        Self::new(&trusted_peers, dev_user.as_deref().map(str::trim))
    }

    /// The user of a request from `peer` with `headers`.
    fn resolve(
        &self,
        peer: Option<IpAddr>,
        headers: &HeaderMap,
    ) -> Result<Option<AbaUser>, IdentityRejection> {
        let language = self.0.language;
        let trusted = peer.is_some_and(|ip| self.0.trusted_peers.contains(&ip.to_canonical()));
        let mut ids = headers.get_all(USER_HEADER).iter();
        if trusted && let Some(value) = ids.next() {
            // Two values mean the proxy appended its header to the client's instead of
            // replacing it: refuse, rather than believe the first one.
            if ids.next().is_some() {
                return Err(IdentityRejection::Invalid(language));
            }
            let id = value.to_str().unwrap_or("").trim();
            if !valid_id(id) {
                return Err(IdentityRejection::Invalid(language));
            }
            let name = headers
                .get(NAME_HEADER)
                .and_then(|value| value.to_str().ok())
                .and_then(display_name)
                .unwrap_or_else(|| id.to_owned());
            // The same rule as for the user: two lists mean a client's list got through.
            let mut lists = headers.get_all(APPS_HEADER).iter();
            let apps = match (lists.next(), lists.next()) {
                (None, _) => None,
                (Some(_), Some(_)) => return Err(IdentityRejection::Invalid(language)),
                (Some(value), None) => Some(value.to_str().map(app_list).unwrap_or_default()),
            };
            return Ok(Some(AbaUser {
                id: id.to_owned(),
                name,
                apps,
            }));
        }
        Ok(self.0.dev_user.as_ref().map(|id| AbaUser {
            id: id.clone(),
            name: id.clone(),
            apps: None,
        }))
    }
}

/// Why a request has no usable user, and the [`Language`] of the JSON error that says so (the
/// one the app gave its [`Identity`] with [`Identity::with_language`], German without).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityRejection {
    /// No trusted identity header and no dev user: 401.
    Missing(Language),
    /// A trusted `X-Aba-User` that is not one valid user id: 400.
    Invalid(Language),
}

impl IntoResponse for IdentityRejection {
    fn into_response(self) -> Response {
        match self {
            IdentityRejection::Missing(language) => crate::error_response(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                language.not_signed_in(),
            ),
            IdentityRejection::Invalid(language) => crate::error_response(
                StatusCode::BAD_REQUEST,
                "bad_request",
                language.invalid_user(),
            ),
        }
    }
}

/// `Option<AbaUser>` as a handler argument: None without a user, 400 for a malformed one.
impl<S> OptionalFromRequestParts<S> for AbaUser
where
    S: Send + Sync,
    Identity: FromRef<S>,
{
    type Rejection = IdentityRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        // The extractor also sees `MockConnectInfo` (tests); no peer address means "not trusted".
        let peer = ConnectInfo::<SocketAddr>::from_request_parts(parts, state)
            .await
            .ok()
            .map(|ConnectInfo(addr)| addr.ip());
        Identity::from_ref(state).resolve(peer, &parts.headers)
    }
}

/// `AbaUser` as a handler argument: 401 without a user, 400 for a malformed one.
impl<S> FromRequestParts<S> for AbaUser
where
    S: Send + Sync,
    Identity: FromRef<S>,
{
    type Rejection = IdentityRejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        <AbaUser as OptionalFromRequestParts<S>>::from_request_parts(parts, state)
            .await?
            .ok_or_else(|| IdentityRejection::Missing(Identity::from_ref(state).0.language))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::{
        Router,
        body::Body,
        extract::connect_info::MockConnectInfo,
        http::{HeaderValue, Request},
        routing::get,
    };
    use tower::ServiceExt;

    use super::*;
    use crate::test_util::body_string;

    const LOOPBACK: &str = "127.0.0.1:50000";
    /// A peer that is not trusted (a documentation address, RFC 5737).
    const STRANGER: &str = "198.51.100.7:50000";

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn ip(addr: &str) -> Option<IpAddr> {
        Some(addr.parse().unwrap())
    }

    fn user(id: &str, name: &str) -> AbaUser {
        AbaUser {
            id: id.to_owned(),
            name: name.to_owned(),
            apps: None,
        }
    }

    fn apps_of(identity: &Identity, peer: &str, pairs: &[(&str, &str)]) -> Option<Vec<String>> {
        identity
            .resolve(ip(peer), &headers(pairs))
            .unwrap()
            .unwrap()
            .apps
    }

    #[test]
    fn the_app_list_comes_from_a_trusted_peer_only() {
        let identity = Identity::default();
        let anna = ("x-aba-user", "anna");
        assert_eq!(
            apps_of(
                &identity,
                "127.0.0.1",
                &[anna, ("x-aba-apps", "notes,photos, wiki ,")]
            ),
            Some(vec!["notes".into(), "photos".into(), "wiki".into()])
        );
        // An app named twice counts once; an empty header is an empty list, not "no list".
        assert_eq!(
            apps_of(
                &identity,
                "127.0.0.1",
                &[anna, ("x-aba-apps", "notes,notes")]
            ),
            Some(vec!["notes".into()])
        );
        assert_eq!(
            apps_of(&identity, "127.0.0.1", &[anna, ("x-aba-apps", "")]),
            Some(vec![])
        );
        // No header: no list at all.
        assert_eq!(apps_of(&identity, "127.0.0.1", &[anna]), None);

        // From an untrusted peer the whole identity is ignored, the list with it.
        let dev = Identity::new(&loopback(), Some("default")).unwrap();
        assert_eq!(
            apps_of(
                &dev,
                "198.51.100.7",
                &[anna, ("x-aba-apps", "notes,photos")]
            ),
            None
        );
        // The dev user never has a list, even when a client sends one without a user.
        assert_eq!(
            apps_of(&dev, "127.0.0.1", &[("x-aba-apps", "notes,photos")]),
            None
        );
    }

    #[test]
    fn a_broken_app_list_grants_nothing() {
        let identity = Identity::default();
        let anna = ("x-aba-user", "anna");
        for bad in [
            "notes,Photos",
            "notes,pho tos",
            "notes;photos",
            "notes,*",
            "n%C3%B6tes",
        ] {
            assert_eq!(
                apps_of(&identity, "127.0.0.1", &[anna, ("x-aba-apps", bad)]),
                Some(vec![]),
                "{bad:?}"
            );
        }
        let too_long = format!("notes,{}", "x".repeat(33));
        assert_eq!(
            apps_of(&identity, "127.0.0.1", &[anna, ("x-aba-apps", &too_long)]),
            Some(vec![])
        );
        // Two lists mean the proxy appended its header to a client's: refused like two users.
        let twice = headers(&[anna, ("x-aba-apps", "notes"), ("x-aba-apps", "photos")]);
        assert_eq!(
            identity.resolve(ip("127.0.0.1"), &twice),
            Err(IdentityRejection::Invalid(Language::German))
        );
    }

    #[test]
    fn a_trusted_peer_names_the_user() {
        let identity = Identity::default();
        let got = identity.resolve(ip("127.0.0.1"), &headers(&[("x-aba-user", "aba")]));
        assert_eq!(got, Ok(Some(user("aba", "aba"))));
        // IPv6 loopback and the IPv4-mapped form of loopback are loopback too.
        for peer in ["::1", "::ffff:127.0.0.1"] {
            let got = identity.resolve(ip(peer), &headers(&[("x-aba-user", " aba ")]));
            assert_eq!(got, Ok(Some(user("aba", "aba"))), "{peer}");
        }
    }

    #[test]
    fn a_forged_header_from_an_untrusted_peer_is_ignored() {
        let identity = Identity::default();
        let forged = headers(&[("x-aba-user", "aba"), ("x-aba-name", "Aba")]);
        assert_eq!(identity.resolve(ip("198.51.100.7"), &forged), Ok(None));
        assert_eq!(identity.resolve(ip("203.0.113.10"), &forged), Ok(None));
        // No peer address at all (the server was started without connect info): not trusted.
        assert_eq!(identity.resolve(None, &forged), Ok(None));
        // Not even a malformed one is looked at.
        let malformed = headers(&[("x-aba-user", "a b")]);
        assert_eq!(identity.resolve(ip("198.51.100.7"), &malformed), Ok(None));
    }

    #[test]
    fn only_the_listed_addresses_are_trusted() {
        let proxy: IpAddr = "192.0.2.1".parse().unwrap();
        let identity = Identity::new(&[proxy], None).unwrap();
        let aba = headers(&[("x-aba-user", "aba")]);
        assert_eq!(
            identity.resolve(Some(proxy), &aba),
            Ok(Some(user("aba", "aba")))
        );
        // Loopback is not on this list, and neither is the proxy's neighbour.
        assert_eq!(identity.resolve(ip("127.0.0.1"), &aba), Ok(None));
        assert_eq!(identity.resolve(ip("192.0.2.2"), &aba), Ok(None));
        // An empty list trusts nobody.
        let nobody = Identity::new(&[], None).unwrap();
        assert_eq!(nobody.resolve(ip("127.0.0.1"), &aba), Ok(None));
    }

    #[test]
    fn the_name_is_percent_decoded_and_falls_back_to_the_id() {
        let identity = Identity::default();
        let peer = ip("127.0.0.1");
        let named = |name: &str| {
            identity
                .resolve(
                    peer,
                    &headers(&[("x-aba-user", "aba"), ("x-aba-name", name)]),
                )
                .unwrap()
                .unwrap()
                .name
        };
        assert_eq!(named("Aba"), "Aba");
        assert_eq!(named("Zo%C3%AB%20Example"), "Zoë Example");
        // Not UTF-8, empty, a control character, too long: the id stands in.
        assert_eq!(named("%FF%FE"), "aba");
        assert_eq!(named("%20"), "aba");
        assert_eq!(named("a%0Ab"), "aba");
        let too_long = "x".repeat(65);
        assert_eq!(named(&too_long), "aba");
    }

    #[test]
    fn a_malformed_user_id_is_an_error() {
        let identity = Identity::default();
        let peer = ip("127.0.0.1");
        let (too_long, longest) = ("x".repeat(65), "x".repeat(64));
        for bad in ["a b", "anna/../x", "", "ä", too_long.as_str()] {
            let got = identity.resolve(peer, &headers(&[("x-aba-user", bad)]));
            assert_eq!(
                got,
                Err(IdentityRejection::Invalid(Language::German)),
                "{bad:?}"
            );
        }
        for good in [
            "aba",
            "anna.b",
            "a_b-c",
            "aba@example.org",
            longest.as_str(),
        ] {
            let got = identity.resolve(peer, &headers(&[("x-aba-user", good)]));
            assert_eq!(got, Ok(Some(user(good, good))), "{good:?}");
        }
    }

    #[test]
    fn two_user_headers_are_refused() {
        let identity = Identity::default();
        let twice = headers(&[("x-aba-user", "mallory"), ("x-aba-user", "aba")]);
        assert_eq!(
            identity.resolve(ip("127.0.0.1"), &twice),
            Err(IdentityRejection::Invalid(Language::German))
        );
    }

    #[test]
    fn the_dev_user_stands_in_where_no_trusted_header_is() {
        let identity = Identity::new(&loopback(), Some("default")).unwrap();
        let none = HeaderMap::new();
        let aba = headers(&[("x-aba-user", "aba")]);
        assert_eq!(
            identity.resolve(ip("127.0.0.1"), &none),
            Ok(Some(user("default", "default")))
        );
        // An untrusted peer's header is ignored, so it is the dev user as well.
        assert_eq!(
            identity.resolve(ip("198.51.100.7"), &aba),
            Ok(Some(user("default", "default")))
        );
        // A trusted header wins.
        assert_eq!(
            identity.resolve(ip("127.0.0.1"), &aba),
            Ok(Some(user("aba", "aba")))
        );
        assert!(Identity::new(&loopback(), Some("a b")).is_err());
    }

    #[test]
    fn settings_come_from_the_environment() {
        let lookup = |pairs: &[(&str, &str)]| {
            let map: HashMap<String, String> = pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect();
            Identity::from_lookup(move |key| map.get(key).cloned())
        };
        let aba = headers(&[("x-aba-user", "aba")]);

        // Unset or empty: loopback, no dev user.
        let unset: &[(&str, &str)] = &[];
        let empty: &[(&str, &str)] = &[("ABA_TRUSTED_PEERS", " "), ("ABA_DEV_USER", "")];
        for pairs in [unset, empty] {
            let identity = lookup(pairs).unwrap();
            assert!(identity.resolve(ip("::1"), &aba).unwrap().is_some());
            assert_eq!(identity.resolve(ip("::1"), &HeaderMap::new()), Ok(None));
        }

        let identity = lookup(&[
            ("ABA_TRUSTED_PEERS", "192.0.2.1, 2001:db8::1"),
            ("ABA_DEV_USER", " dev "),
        ])
        .unwrap();
        for peer in ["192.0.2.1", "2001:db8::1"] {
            assert_eq!(
                identity.resolve(ip(peer), &aba),
                Ok(Some(user("aba", "aba")))
            );
        }
        assert_eq!(
            identity.resolve(ip("127.0.0.1"), &aba),
            Ok(Some(user("dev", "dev"))),
            "loopback is not trusted once a list is given"
        );

        // Ranges and host names are refused: the list is exact addresses.
        for bad in ["192.0.2.0/24", "localhost", "127.0.0.1;::1", ","] {
            assert!(lookup(&[("ABA_TRUSTED_PEERS", bad)]).is_err(), "{bad}");
        }
        assert!(lookup(&[("ABA_DEV_USER", "a b")]).is_err());
    }

    /// Both extractors on a real router, with the peer address the server would see.
    fn app(identity: Identity, peer: Option<&str>) -> Router {
        let router = Router::new()
            .route("/who", get(|user: AbaUser| async move { user.id }))
            .route(
                "/maybe",
                get(|user: Option<AbaUser>| async move {
                    user.map_or_else(|| "nobody".to_owned(), |u| format!("{}/{}", u.id, u.name))
                }),
            )
            .with_state(identity);
        match peer {
            Some(peer) => router.layer(MockConnectInfo(peer.parse::<SocketAddr>().unwrap())),
            None => router,
        }
    }

    async fn call(app: Router, uri: &str, user: Option<&str>) -> (StatusCode, String) {
        let mut request = Request::builder().uri(uri);
        if let Some(user) = user {
            request = request.header(USER_HEADER, user).header(NAME_HEADER, "Aba");
        }
        let response = app
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        (response.status(), body_string(response).await)
    }

    #[tokio::test]
    async fn the_extractors_answer_200_401_and_400() {
        let trusted = || app(Identity::default(), Some(LOOPBACK));
        assert_eq!(
            call(trusted(), "/who", Some("aba")).await,
            (StatusCode::OK, "aba".to_owned())
        );
        assert_eq!(
            call(trusted(), "/maybe", Some("aba")).await,
            (StatusCode::OK, "aba/Aba".to_owned())
        );

        // Without the header: 401 for the required user, None for the optional one.
        let (status, body) = call(trusted(), "/who", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["code"], "unauthorized");
        assert_eq!(json["details"], serde_json::json!([]));
        assert_eq!(
            call(trusted(), "/maybe", None).await,
            (StatusCode::OK, "nobody".to_owned())
        );

        // A forged header from an untrusted peer, or with no peer address: nobody.
        for peer in [Some(STRANGER), None] {
            let forged = || app(Identity::default(), peer);
            assert_eq!(
                call(forged(), "/who", Some("aba")).await.0,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                call(forged(), "/maybe", Some("aba")).await,
                (StatusCode::OK, "nobody".to_owned())
            );
        }

        // A malformed id is a 400 for both.
        for uri in ["/who", "/maybe"] {
            let (status, body) = call(trusted(), uri, Some("a b")).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            let json: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(json["code"], "bad_request");
        }

        // The dev user answers where the header is missing or not trusted.
        let dev = || {
            app(
                Identity::new(&loopback(), Some("default")).unwrap(),
                Some(STRANGER),
            )
        };
        assert_eq!(
            call(dev(), "/who", Some("aba")).await,
            (StatusCode::OK, "default".to_owned())
        );
    }

    /// An app's own state works through `FromRef`.
    #[tokio::test]
    async fn an_app_state_hands_out_the_identity() {
        #[derive(Clone)]
        struct AppState {
            identity: Identity,
        }
        impl FromRef<AppState> for Identity {
            fn from_ref(state: &AppState) -> Identity {
                state.identity.clone()
            }
        }
        let app_with = |identity: Identity| {
            Router::new()
                .route("/who", get(|user: AbaUser| async move { user.name }))
                .with_state(AppState { identity })
                .layer(MockConnectInfo(LOOPBACK.parse::<SocketAddr>().unwrap()))
        };
        assert_eq!(
            call(app_with(Identity::default()), "/who", Some("aba")).await,
            (StatusCode::OK, "Aba".to_owned())
        );
        // The language the app gave its identity reaches the 401 through the state.
        let english = Identity::default().with_language(Language::English);
        let (status, body) = call(app_with(english), "/who", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("Not signed in"), "{body}");
    }

    /// The message of both JSON errors, in the language the identity was given.
    #[tokio::test]
    async fn the_error_messages_follow_the_language() {
        let german = (
            "Nicht angemeldet",
            "Ungültiger Benutzer im X-Aba-User-Header",
        );
        let english = ("Not signed in", "Invalid user in the X-Aba-User header");
        for (identity, (unauthorized, invalid)) in [
            // Nothing set: German, as before the setting existed.
            (Identity::default(), german),
            (Identity::default().with_language(Language::German), german),
            (
                Identity::default().with_language(Language::English),
                english,
            ),
        ] {
            let trusted = || app(identity.clone(), Some(LOOPBACK));
            let error = |body: &str| serde_json::from_str::<serde_json::Value>(body).unwrap();

            let (status, body) = call(trusted(), "/who", None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(
                error(&body),
                serde_json::json!({"code": "unauthorized", "message": unauthorized, "details": []})
            );

            for uri in ["/who", "/maybe"] {
                let (status, body) = call(trusted(), uri, Some("a b")).await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
                assert_eq!(
                    error(&body),
                    serde_json::json!({"code": "bad_request", "message": invalid, "details": []}),
                    "{uri}"
                );
            }
        }
    }

    #[test]
    fn the_language_changes_no_rule_and_no_other_identity() {
        let proxy: IpAddr = "192.0.2.1".parse().unwrap();
        let aba = headers(&[("x-aba-user", "aba")]);
        let twice = headers(&[("x-aba-user", "mallory"), ("x-aba-user", "aba")]);

        let plain = Identity::new(&[proxy], Some("dev")).unwrap();
        let english = plain.clone().with_language(Language::English);
        for identity in [&plain, &english] {
            // The same trusted peers, the same dev user.
            assert_eq!(
                identity.resolve(Some(proxy), &aba),
                Ok(Some(user("aba", "aba")))
            );
            assert_eq!(
                identity.resolve(ip("127.0.0.1"), &aba),
                Ok(Some(user("dev", "dev")))
            );
        }
        // Only the rejection differs, and the identity it was made from keeps its language.
        assert_eq!(
            english.resolve(Some(proxy), &twice),
            Err(IdentityRejection::Invalid(Language::English))
        );
        assert_eq!(
            plain.resolve(Some(proxy), &twice),
            Err(IdentityRejection::Invalid(Language::German))
        );
        // It also works on an identity read from the environment.
        let from_env = Identity::from_lookup(|_| None)
            .unwrap()
            .with_language(Language::English);
        assert_eq!(
            from_env.resolve(ip("127.0.0.1"), &twice),
            Err(IdentityRejection::Invalid(Language::English))
        );
    }
}
