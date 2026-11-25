//! Every piece of aba-kit in one small app: a themed page that shows who you are and counts
//! your greetings in SQLite.
//!
//! ```text
//! cargo run --example hello
//! ```
//!
//! It listens on 127.0.0.1:8080 (`PORT` changes the port) and serves `/`, `POST /gruss`,
//! `/healthz`, `/api/version` and `/aba/aba-theme.css`. `hello --healthcheck` probes a running
//! instance and exits 0 or 1, as an image's HEALTHCHECK would run it.
//!
//! The page knows you when the request carries `X-Aba-User` and comes from a trusted peer
//! (loopback by default, `ABA_TRUSTED_PEERS` otherwise); `ABA_DEV_USER=dev` gives every other
//! request a user. The database lives in `DATA_DIR` (default: `aba-kit-hello` in the temp
//! directory).

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, Mutex, PoisonError},
};

use aba_kit::{
    AbaUser, Identity,
    http::{self, VersionInfo},
    sqlite, telemetry,
    theme::{self, NavLink, Shell},
};
use axum::{
    Router,
    extract::{FromRef, State},
    http::{StatusCode, header},
    middleware,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use maud::{PreEscaped, html};
use rusqlite::Connection;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

/// What every page of this app has in common.
const SHELL: Shell = Shell {
    app: "Hello",
    nav: &[NavLink {
        href: "/",
        label: "Start",
    }],
    head: PreEscaped(""),
};

/// The schema: entry `i` takes the database from version `i` to `i + 1`. Append only.
const MIGRATIONS: &[&str] = &["CREATE TABLE greetings (
       user_id  TEXT PRIMARY KEY,
       count    INTEGER NOT NULL
     );"];

#[derive(Clone)]
struct AppState {
    identity: Identity,
    db: Arc<Mutex<Connection>>,
}

/// This is what lets `AbaUser` be a handler argument.
impl FromRef<AppState> for Identity {
    fn from_ref(state: &AppState) -> Identity {
        state.identity.clone()
    }
}

impl AppState {
    /// Runs `f` on the one connection, on a blocking thread.
    async fn db<T, F>(&self, f: F) -> Result<T, StatusCode>
    where
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let db = self.db.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let conn = db.lock().unwrap_or_else(PoisonError::into_inner);
            f(&conn)
        })
        .await;
        match outcome {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => {
                tracing::error!(%error, "database error");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
            Err(error) => {
                tracing::error!(%error, "database task failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }
}

/// `GET /`: the page. `Option<AbaUser>` lets it also answer a request without a user.
async fn start(
    State(state): State<AppState>,
    user: Option<AbaUser>,
) -> Result<Response, StatusCode> {
    let greetings = match &user {
        None => 0,
        Some(user) => {
            let id = user.id.clone();
            state
                .db(move |conn| {
                    conn.query_row(
                        "SELECT coalesce(sum(count), 0) FROM greetings WHERE user_id = ?1",
                        [id],
                        |row| row.get::<_, i64>(0),
                    )
                })
                .await?
        }
    };
    let content = html! {
        div class="page-head" {
            h1 { "Hallo" }
            p class="sub" { "Die Bausteine aus aba-kit auf einer Seite." }
        }
        @match &user {
            Some(user) => {
                div class="card" {
                    h2 { "Angemeldet" }
                    dl class="kv" {
                        div { dt { "Benutzer" } dd { (user.id) } }
                        div { dt { "Name" } dd { (user.name) } }
                        div { dt { "Grüße" } dd { (greetings) } }
                    }
                    form class="btn-row" method="post" action="/gruss" {
                        button class="btn primary" type="submit" { "Grüßen" }
                    }
                }
            }
            None => {
                p class="note" {
                    "Nicht angemeldet: kein X-Aba-User von einem vertrauten Absender."
                }
            }
        }
    };
    // A page about one user is never stored by a cache.
    let no_store = [(header::CACHE_CONTROL, "no-store")];
    Ok((no_store, SHELL.page("Start", "/", content)).into_response())
}

/// `POST /gruss`: a write. `AbaUser` answers 401 without a user; the guard in front of the
/// router has already refused the request if another site sent it.
async fn greet(State(state): State<AppState>, user: AbaUser) -> Result<Redirect, StatusCode> {
    state
        .db(move |conn| {
            conn.execute(
                "INSERT INTO greetings (user_id, count) VALUES (?1, 1)
                 ON CONFLICT (user_id) DO UPDATE SET count = count + 1",
                [user.id],
            )
        })
        .await?;
    Ok(Redirect::to("/"))
}

fn app(state: AppState) -> Router {
    let version = VersionInfo::new(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        option_env!("GIT_SHA"),
    );
    let logged = Router::new()
        .route("/", get(start))
        .route("/gruss", post(greet))
        .with_state(state)
        .merge(theme::router())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        );
    // /healthz stays out of the request log: a container health check calls it on a timer.
    Router::new()
        .merge(http::health_router(version))
        .merge(logged)
        .layer(middleware::from_fn(http::reject_cross_site_writes))
        .layer(middleware::from_fn(http::security_headers))
}

async fn serve(addr: SocketAddr) -> anyhow::Result<()> {
    let identity = Identity::from_env().map_err(anyhow::Error::msg)?;
    let data_dir = std::env::var_os("DATA_DIR")
        .map_or_else(|| std::env::temp_dir().join("aba-kit-hello"), PathBuf::from);
    std::fs::create_dir_all(&data_dir)?;
    let conn = sqlite::open(&data_dir.join("hello.db"), MIGRATIONS)?;
    let state = AppState {
        identity: identity.clone(),
        db: Arc::new(Mutex::new(conn)),
    };

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, data_dir = %data_dir.display(), ?identity, "listening");
    // The peer address is what the identity headers are judged by.
    axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn main() -> ExitCode {
    let port = match std::env::var("PORT").ok().filter(|p| !p.trim().is_empty()) {
        None => 8080,
        Some(raw) => match raw.trim().parse() {
            Ok(port) => port,
            Err(_) => {
                eprintln!("PORT must be a port number, got {raw:?}");
                return ExitCode::from(2);
            }
        },
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("--healthcheck") => {
            return if http::healthcheck(addr) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Some(other) => {
            eprintln!("unknown argument {other:?}; the only one is --healthcheck");
            return ExitCode::from(2);
        }
    }

    telemetry::init();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "cannot start the async runtime");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(serve(addr)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = format!("{error:#}"), "server stopped with an error");
            ExitCode::FAILURE
        }
    }
}
