# AbaKit

The repo is AbaKit; the crate inside is called `aba-kit`, as Rust crates are named, and is used as
`aba_kit` in code.

The pieces a small Rust web app behind a sign-on proxy needs, so that the next app does not write
them again. They were taken out of an existing app's server (axum, maud, SQLite), not redesigned.

| Module | What an app gets |
|---|---|
| `identity` | `AbaUser`: the user the sign-on proxy names in `X-Aba-User` |
| `http` | the security headers, the guard against cross-site writes, `/healthz` and `/api/version` |
| `theme` | `aba-theme.css` at `/aba/aba-theme.css` and the page shell for maud |
| `telemetry` | the `tracing` setup |
| `sqlite` | open a database with the house pragmas, migrate it by `PRAGMA user_version` |
| `Language` (crate root) | German (the default) or English for the six texts aba-kit writes itself |

**The rule for this crate:** the API stays small, and every change is checked against the apps
that use it before it is committed.

Built and tested with stable Rust 1.97.1 (edition 2024).

**Licence:** none is granted yet (`license = "UNLICENSED"`): all rights reserved for now.

## Add it to an app

```toml
[dependencies]
aba-kit = { git = "https://github.com/Abakus2k/AbaKit", branch = "main" }
```

`Cargo.lock` pins the commit it fetched; `cargo update -p aba-kit` moves it to the newest `main`.
To pin one commit, write `rev = "<commit>"` instead of `branch`. The repository is public, so this
needs no token and no key: a CI runner and an image build fetch it like any other git dependency.

To work on the crate next to an app, point the app at a checkout instead (a path dependency only
builds where that checkout is):

```toml
aba-kit = { path = "../AbaKit" }
```

The app must use the same minor versions of `axum` (0.8), `maud` (0.27) and `rusqlite` (0.43) as
aba-kit, because their types cross the API.

## The whole thing in one file

`examples/hello.rs` is a complete small app with every piece: a themed page that shows who you
are and counts your greetings in SQLite. Every snippet below is taken from it, except the
English one under "Language", which is the doc test of `Language`. The example sets no
language, so its texts are German.

```sh
cargo run --example hello
```

Then open <http://127.0.0.1:8080/>. Without a sign-on proxy the page says "Nicht angemeldet". To be
someone, start it with a dev user:

```sh
ABA_DEV_USER=aba cargo run --example hello
```

(in PowerShell: `$env:ABA_DEV_USER = 'aba'; cargo run --example hello`).

## identity: who is asking

No app stores passwords or a user table. A sign-on proxy in front of the app names the user in
request headers, and the app believes them only from that proxy:

- `X-Aba-User`: the stable user id, for example `aba`. 1 to 64 characters out of ASCII letters,
  digits and `-`, `_`, `.`, `@`. It keys the user's rows, the same in every app.
- `X-Aba-Name`: the display name, percent-encoded UTF-8. Optional; the id stands in for it.
- `X-Aba-Apps`: the app ids this person may use, separated by commas (`notes,photos,wiki`), as
  the proxy knows them. Optional. `AbaUser::apps` is `None` without it and for the dev user; an
  app that shows what a person may use (a start page with a tile per app) shows nothing then.
  One part that is not an app id (lower-case letters, digits, `-`, at most 32) drops the whole
  list to `Some(vec![])`, and two `X-Aba-Apps` headers are refused with 400, as two users are.

Put an `Identity` into the app's state and tell axum where it is:

```rust
use aba_kit::{AbaUser, Identity};
use axum::extract::FromRef;

#[derive(Clone)]
struct AppState {
    identity: Identity,
    // ...
}

impl FromRef<AppState> for Identity {
    fn from_ref(state: &AppState) -> Identity {
        state.identity.clone()
    }
}

let identity = Identity::from_env().map_err(anyhow::Error::msg)?;
```

Then a handler takes the user as an argument:

```rust
// 401 without a user, 400 for a malformed X-Aba-User.
async fn greet(State(state): State<AppState>, user: AbaUser) -> Result<Redirect, StatusCode> { /* user.id */ }

// None without a user: for pages that also answer strangers.
async fn start(State(state): State<AppState>, user: Option<AbaUser>) -> Result<Response, StatusCode> { /* ... */ }
```

The server must be started with the peer address, or nobody is trusted:

```rust
axum::serve(listener, app(state).into_make_service_with_connect_info::<SocketAddr>()).await?;
```

| Variable | Meaning | Default |
|---|---|---|
| `ABA_TRUSTED_PEERS` | The addresses whose identity headers count: exact IPs separated by commas. No ranges, no host names. Behind a proxy this is the address the proxy's connections arrive from, as the app sees it (in a container network, the proxy's container or the network's gateway) | `127.0.0.1,::1` |
| `ABA_DEV_USER` | The user of every request without a trusted header: for local runs and demo builds. **Never set it on the server** | none |

A header from any other peer is ignored, so a client cannot name itself. Two `X-Aba-User` headers
in one request are refused with 400: it means the proxy appended its header instead of replacing
the client's.

In tests, give the router a peer address with `axum::extract::connect_info::MockConnectInfo`.

`identity::USER_HEADER` (and `NAME_HEADER`, `APPS_HEADER`) are public on purpose: an app that asks
another app behind the same proxy for something on behalf of the person whose page it renders
sends the same `X-Aba-User`. The receiving app believes it for the same reason it believes the
proxy: the caller's address is one of its `ABA_TRUSTED_PEERS`. Whether that is enough or a shared
secret header should be added is still open.

## http: headers, the CSRF guard, health

```rust
use aba_kit::http::{self, VersionInfo};
use axum::{Router, middleware};

let version = VersionInfo::new(
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_VERSION"),
    option_env!("GIT_SHA"),
);
Router::new()
    .merge(http::health_router(version))
    .merge(logged) // the app's own routes
    .layer(middleware::from_fn(http::reject_cross_site_writes))
    .layer(middleware::from_fn(http::security_headers))
```

- **`security_headers`** sets five headers on every response: `Content-Security-Policy`
  (`http::CONTENT_SECURITY_POLICY`: own origin only, no inline script or style),
  `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY` and
  `X-Robots-Tag: noindex, nofollow`. Put it outermost.
- **`reject_cross_site_writes`** is the CSRF guard. **It is a rule, not an option**, for every app
  that takes its user from `identity`: the identity comes from the network, so without the guard
  any web page the user opens could change the app's state. Every method but GET, HEAD and
  OPTIONS is a write, and what the browser itself says decides first: `Sec-Fetch-Site`
  `same-origin` or `none` passes whatever `Origin` says; `cross-site`, `same-site`, any other
  value and the header twice answer 403. Only a request without `Sec-Fetch-Site` (an old browser,
  curl) is judged by its `Origin`: none or the app's own host passes, another host, another port
  and `null` answer 403. `Origin` cannot decide for a browser: under `Referrer-Policy:
  no-referrer` a browser sends `Origin: null` with the app's own form POST (the doc comment of
  `reject_cross_site_writes` has the whole table and why it is safe). The other half of the rule
  is the app's: **a GET never changes state.**
- **`health_router`** serves `GET /healthz` (the text `ok`) and `GET /api/version`
  (`{"name", "version", "git_sha"}`), both `Cache-Control: no-store`. The commit is the `GIT_SHA`
  of the process environment, else the one baked in at build time (`GIT_SHA=... cargo build`).
- **`healthcheck(addr)`** is for an image's `HEALTHCHECK`. A minimal runtime image has no shell
  and no curl, so the binary asks itself:

```rust
Some("--healthcheck") => {
    return if http::healthcheck(addr) { ExitCode::SUCCESS } else { ExitCode::FAILURE };
}
```

An error this crate answers with (400, 401, 403) has one JSON body:
`{"code": "forbidden", "message": "...", "details": []}`. The message is German unless the app
sets a [language](#language-the-six-texts-aba-kit-writes-itself).

## theme: one look

`aba-theme.css` holds the design tokens (custom properties on `:root`, light and dark by
`prefers-color-scheme`), the base rules, the shell with its navigation (bottom tab bar on phones,
side navigation from 900 px), and the generic building blocks: `.card`, `.page-head`, `.eyebrow`,
`.btn` (`.primary`, `.danger`, `.ghost`, `.big`), `.btn-row`, `.icon-btn`, `.input`, `label.field`,
`.field-group`, `.badge`, `.note`, `.error-box`, `.kv`. It needs nothing from another origin.

**Long words.** A text with one long unbroken word (a path, a web address, a long compound) breaks
inside the page; it does not make the page wider than the phone, which would also push the fixed
tab bar's right-hand tabs off the screen. `.main` has `overflow-wrap: anywhere`, which everything in
the shell's main element inherits (headings, paragraphs, list lines, labels). A `.kv` pair keeps a
label and its value on one line when they fit and puts them on two lines when they do not, so a
short label is never squeezed apart. Put your own markup in the shell and it follows the same rule.

**Your own flex rows need one of two remedies.** `overflow-wrap: anywhere` lowers the minimum width
of every flex item in `.main` to one letter, not only of the items that hold a long word. In a row
of your own that does not wrap, a short label or a `.btn` next to an ordinary multi-word sentence is
squeezed until it breaks apart (measured at 320 and 390 px: "Gelesen" becomes G/el/es/e/n, and
"Bearbeiten" becomes Be/ar/bei/ten). Prevent it in one of two ways:

- `flex-wrap: wrap` on the row: the label and the text go onto two lines when they do not fit on one, or
- `flex-shrink: 0` on the label or button side: it keeps its width and the text gives way.

`min-width: 0` on the free-text side does **not** prevent it (measured: the label is still broken
apart); it is only the right rule for the value of a `.kv` pair, which also wraps. The alternative
`overflow-wrap: break-word` on `.main` would leave flex items alone, but then every flex item that
holds a long word needs its own `min-width: 0` or the page widens again, which is the problem this
rule solves, so the theme keeps `anywhere`. `scripts/measure-layout.ps1` has all three cases as
fixture rows (see "Checks"); for an app's own pages use `-NoFixture`.

Serve it:

```rust
use aba_kit::theme;

let logged = Router::new()
    .route("/", get(start))
    .with_state(state)
    .merge(theme::router()); // GET /aba/aba-theme.css
```

**A maud app** wraps every view in the shell, which also links the stylesheet:

```rust
use aba_kit::theme::{NavLink, Shell};
use maud::{PreEscaped, html};

const SHELL: Shell = Shell {
    app: "Hello",
    nav: &[NavLink { href: "/", label: "Start" }],
    // More for <head>, written as it is: manifest, icons, the app's own stylesheet.
    head: PreEscaped(""),
};

let content = html! {
    div class="page-head" { h1 { "Hallo" } }
    div class="card" {
        dl class="kv" {
            div { dt { "Benutzer" } dd { (user.id) } }
            div { dt { "Name" } dd { (user.name) } }
        }
    }
};
SHELL.page("Start", "/", content) // title "Start · Hello"; "/" is the request path
```

The three texts the shell writes itself (`lang`, the skip link, the navigation's `aria-label`)
are German; `SHELL.page_in(Language::English, "Start", "/", content)` writes them in English
(see "Language" below).

**A Leptos app** links the stylesheet in its `index.html`, in front of its own:

```html
<link rel="stylesheet" href="/aba/aba-theme.css" />
```

and renders the same markup as the shell (`.skip-link`, `.shell`, `.nav`, `.main`, `.page`).

An app's own stylesheet may add tokens and classes. It should not redefine the shared ones.

## Language: the six texts aba-kit writes itself

aba-kit writes six short texts that a person or a client reads. `Language` (crate root) is German
or English for exactly these, and for nothing else; every other text on a page is the app's.

| Where | German (the default) | English |
|---|---|---|
| `lang` attribute of `<html>` (page shell) | `de` | `en` |
| the skip link (page shell) | Zum Inhalt springen | Skip to content |
| `aria-label` of the navigation (page shell) | Hauptnavigation | Main navigation |
| 401 of `AbaUser`: `message` of the JSON error | Nicht angemeldet | Not signed in |
| 400 of `AbaUser`: `message` of the JSON error | Ungültiger Benutzer im X-Aba-User-Header | Invalid user in the X-Aba-User header |
| 403 of the cross-site guard: `message` of the JSON error | Änderungen nur aus der App selbst | Changes only from the app itself |

**The default is German, as it always was: an app that sets nothing is unchanged** (the status and
the `code` of an error are the same in both languages). An app that wants English sets it in the
three places that write these texts, with one constant:

```rust
use aba_kit::{Identity, Language, http, theme::Shell};
use axum::{Router, middleware};

const LANGUAGE: Language = Language::English;

// 1. The page shell: lang, skip link and navigation label.
SHELL.page_in(LANGUAGE, "Today", "/", content)

// 2. The identity's 401 and 400 (the identity lives in the app's state).
let identity = Identity::from_env().map_err(anyhow::Error::msg)?.with_language(LANGUAGE);

// 3. The guard against cross-site writes: the same guard, with the language as its state.
Router::new()
    .merge(logged)
    .layer(middleware::from_fn_with_state(LANGUAGE, http::reject_cross_site_writes_in))
    .layer(middleware::from_fn(http::security_headers))
```

`Shell::page`, `http::reject_cross_site_writes` and an `Identity` without `with_language` are the
German forms. `IdentityRejection` carries the language of its identity (`Missing(Language)`,
`Invalid(Language)`). A Leptos client writes its own `lang` and skip link.

## telemetry: logs

```rust
aba_kit::telemetry::init(); // once, first thing in main
```

Lines go to stdout, where a container runtime or a service manager collects them. `RUST_LOG` is
the filter (default `info`); `LOG_FORMAT=json` writes one JSON object per line.

Request spans are tower-http's layer around the app's routes, but not around `/healthz`, which a
container health check calls on a timer:

```rust
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

.layer(
    TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
        .on_response(DefaultOnResponse::new().level(Level::INFO)),
)
```

## sqlite: the app's own database

```rust
/// The schema: entry `i` takes the database from version `i` to `i + 1`. Append only;
/// never edit a shipped entry.
const MIGRATIONS: &[&str] = &["CREATE TABLE greetings (
       user_id  TEXT PRIMARY KEY,
       count    INTEGER NOT NULL
     );"];

let conn = aba_kit::sqlite::open(&data_dir.join("hello.db"), MIGRATIONS)?;
```

`open` creates the file if needed, sets the pragmas (busy timeout 10 s, WAL, `synchronous =
NORMAL`, `foreign_keys = ON`) and runs the missing migrations, each in its own transaction with
its `PRAGMA user_version`. It refuses a database newer than the build. Before it upgrades an
existing database it writes a consistent copy next to it (`hello.db.pre-v2`) and keeps the newest
three, so the previous build can still start after a rollback.

One connection writes. The app keeps it in `Arc<Mutex<Connection>>` and runs queries on blocking
threads (`AppState::db` in the example). Every row is keyed by `AbaUser::id`.

## What is different from the original code

For whoever moves the app this code was taken from onto the crate (a Leptos client and an axum
server):

| Piece | The original app | aba-kit |
|---|---|---|
| Trusted peers | A flag `TRUST_USER_HEADER` plus loopback | The list `ABA_TRUSTED_PEERS`, default loopback. `Identity::new(&[], Some("default"))` is the old "flag off" |
| No user | Always the user `default` | `Option<AbaUser>` is `None` and `AbaUser` answers 401, unless a dev user is set |
| Display name | Not read | `X-Aba-Name` in `AbaUser::name` |
| Two `X-Aba-User` headers | The first one counts | 400 |
| The person's apps | Not read | `X-Aba-Apps` in `AbaUser::apps` (the proxy's list) |
| CSRF guard | Only paths under `/api/` | Every path, so a maud app's form targets are covered |
| CSRF guard, `Sec-Fetch-Site` against `Origin` | A foreign `Origin` (`null` included) is refused whatever `Sec-Fetch-Site` says, so a plain form POST of its own page (`Origin: null` under `no-referrer`) would be refused; its client writes with `fetch()` in `cors` mode, which sends the real origin, so it is not hit | `Sec-Fetch-Site` decides first: `same-origin` and `none` pass whatever `Origin` says, `cross-site`, `same-site` and any other value are refused; only without `Sec-Fetch-Site` does `Origin` decide (`null` refused). Changed because the old rule refused every plain form POST of a maud app |
| `/healthz`, `/api/version` | `no-store` from the cache middleware | They set `no-store` themselves. Merge the router before `method_not_allowed_fallback`, so a wrong method keeps its JSON 405 |
| Navigation columns | Six, fixed | One per tab |
| Long words | A long unbroken word widens the page unless the class has its own `overflow-wrap` (a few do); a `.kv` value like `12,34 €` is squeezed onto two lines at 320 px | `.main` breaks anywhere, and a `.kv` label and value go onto two lines when they do not fit on one (`flex-wrap`, `min-width: 0` on the value). Drop the original's copies of `.kv > div` and `.kv dd` when switching: they would come after the theme and override its `gap` and margin. **Check its other flex rows:** many of them do not wrap and have no `flex-shrink: 0` on the short side, and `.main`'s `overflow-wrap: anywhere` squeezes a short label or button in such a row apart next to an ordinary sentence. After the switch run `scripts/measure-layout.ps1 -NoFixture` on its pages and give each row that breaks `flex-wrap: wrap` or `flex-shrink: 0` on the label or button (see "Long words" under theme) |
| Language of the shell texts and the three JSON error messages | German only | German by default, the same; `Language::English` is a setting (`Shell::page_in`, `Identity::with_language`, `http::reject_cross_site_writes_in`) |
| Upgrade copies | Named with chrono | Same names, no chrono |

## What is not in here

These stay in each app, because they are tied to its own layout: the cache rules and the charset
fix for served files, the static file service with the client-route fallback, compression, the
error type with its extractors, the shutdown signal, icons, and the nightly backup (a server
script).

## Checks

In the repository's folder:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

After a change to `aba-theme.css`, measure the layout in headless Chrome (no window). Start the
example as shown under "The whole thing in one file", then:

```powershell
pwsh -File scripts/measure-layout.ps1 -Url http://127.0.0.1:8080/ -Widths 320,390,900,1280 -ScreenshotDir target/layout
```

The script (it needs Chrome at its usual place on Windows, or `-Chrome`) replaces the page's
content by a fixture with a 60-character web address and a 60-character unbroken word in every
building block, and fails (exit 1) when a width is wider than its viewport, a tab of the bottom
bar is off screen, or a word of 16 characters or less is broken across two lines. The fixture also
holds five flex rows of an app's own, a short label or button next to an ordinary sentence: three
with a remedy (`flex-wrap: wrap`, `flex-shrink: 0` on a label, `flex-shrink: 0` on a button),
which must pass, and two without (none, and `min-width: 0` on the text side), which must squeeze
the label; when they do not, the check is blind or the advice under "Long words" is stale, and the
run fails. With `-NoFixture` it measures the page as it is (an app's own page).
