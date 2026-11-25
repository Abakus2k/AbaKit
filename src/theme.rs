//! The shared look: `aba-theme.css` and the page shell for maud.
//!
//! - [`router`] serves the stylesheet at [`CSS_PATH`]. A Leptos app links it from its
//!   `index.html` in front of its own stylesheet; a maud app gets the link from [`Shell::page`].
//! - [`Shell::page`] is the page every maud view is wrapped in: the head and the markup (skip
//!   link, navigation, main) a Leptos client of the same look renders, so a maud app and a
//!   Leptos app look and behave the same. The three texts the shell writes itself
//!   (the `lang` attribute, the skip link and the label of the navigation) are German by
//!   default; [`Shell::page_in`] writes them in another [`Language`].
//!
//! ```
//! use aba_kit::{
//!     Language,
//!     theme::{NavLink, Shell},
//! };
//! use maud::{PreEscaped, html};
//!
//! const SHELL: Shell = Shell {
//!     app: "Planner",
//!     nav: &[
//!         NavLink { href: "/", label: "Heute" },
//!         NavLink { href: "/aufgaben", label: "Aufgaben" },
//!     ],
//!     head: PreEscaped(r#"<link rel="manifest" href="/manifest.webmanifest">"#),
//! };
//!
//! let page = SHELL.page("Aufgaben", "/aufgaben/42", html! { h1 { "Aufgaben" } });
//! let page = page.into_string();
//! assert!(page.contains("<title>Aufgaben · Planner</title>"));
//! assert!(page.contains(r#"<html lang="de">"#));
//!
//! // The same shell in English: lang, skip link and navigation label change, nothing else.
//! let page = SHELL.page_in(Language::English, "Tasks", "/aufgaben/42", html! { h1 { "Tasks" } });
//! let page = page.into_string();
//! assert!(page.contains(r#"<html lang="en">"#));
//! assert!(page.contains("Skip to content") && page.contains(r#"aria-label="Main navigation""#));
//! ```
//!
//! The stylesheet holds the design tokens (custom properties on `:root`, light and dark by
//! `prefers-color-scheme`), the base rules, the shell and navigation, and the generic building
//! blocks (`.card`, `.page-head`, `.btn`, `.input`, `.badge`, `.note`, `.error-box`, `.kv`).
//! It needs nothing from another origin and no inline style, so it works under
//! [`crate::http::CONTENT_SECURITY_POLICY`].
//!
//! A text with one long unbroken word (a path, a web address, a long compound) breaks inside the
//! page instead of making it wider than the phone: the shell's main element has
//! `overflow-wrap: anywhere`, which every heading, paragraph and label inside it inherits, and
//! a `.kv` label and its value go onto two lines when they do not fit on one. The same rule lets a
//! flex row of an app's own that does not wrap squeeze a short label or button apart, so such a
//! row needs `flex-wrap: wrap`, or `flex-shrink: 0` on its label or button (README, "Long words").

use std::sync::LazyLock;

use axum::{
    Router,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use maud::{DOCTYPE, Markup, PreEscaped, html};

use crate::Language;

/// The stylesheet.
pub const CSS: &str = include_str!("../assets/aba-theme.css");

/// Where [`router`] serves [`CSS`].
pub const CSS_PATH: &str = "/aba/aba-theme.css";

/// Changes with the stylesheet, so a browser revalidates with one cheap request.
static ETAG: LazyLock<String> = LazyLock::new(|| format!("\"{:016x}\"", fnv1a(CSS.as_bytes())));

/// FNV-1a, 64 bit: a content hash for the ETag (not for anything security related).
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// `GET /aba/aba-theme.css`: the stylesheet, `Cache-Control: no-cache` with an ETag (the
/// browser asks every time and gets a 304 while the file is the same).
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new().route(CSS_PATH, get(css))
}

async fn css(headers: HeaderMap) -> Response {
    let etag = ETAG.as_str();
    let unchanged = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .map(|tag| tag.trim().trim_start_matches("W/"))
                .any(|tag| tag == etag || tag == "*")
        });
    let caching = [(header::CACHE_CONTROL, "no-cache"), (header::ETAG, etag)];
    if unchanged {
        (StatusCode::NOT_MODIFIED, caching).into_response()
    } else {
        let content_type = [(header::CONTENT_TYPE, "text/css; charset=utf-8")];
        (caching, content_type, CSS).into_response()
    }
}

/// One entry of the navigation: the bottom tab bar on phones, the side navigation on wide
/// screens (same markup, the stylesheet decides).
#[derive(Debug, Clone, Copy)]
pub struct NavLink<'a> {
    /// Where it leads, for example `/aufgaben`. Also decides when the entry is the current one:
    /// on this path and below it (`/` only on `/` itself).
    pub href: &'a str,
    /// The text of the tab.
    pub label: &'a str,
}

/// What every page of an app has in common. Usually one `const` per app.
#[derive(Debug, Clone, Copy)]
pub struct Shell<'a> {
    /// The app's name: the brand in the navigation and the end of every page title.
    pub app: &'a str,
    /// The navigation entries, at least one.
    pub nav: &'a [NavLink<'a>],
    /// More elements for `<head>`, written as they are: the app's manifest, icons and its own
    /// stylesheet. `PreEscaped("")` for none.
    pub head: PreEscaped<&'a str>,
}

/// Whether the tab `href` is the current one for `path`.
fn nav_active(href: &str, path: &str) -> bool {
    if href == "/" {
        return path == "/";
    }
    path.strip_prefix(href)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

impl Shell<'_> {
    /// The whole HTML page: `content` inside the shell, titled `<title> · <app>`. `path` is the
    /// path of the request; it marks the current navigation entry. The three texts the shell
    /// writes itself are German; [`Shell::page_in`] changes that.
    pub fn page(&self, title: &str, path: &str, content: Markup) -> Markup {
        self.page_in(Language::default(), title, path, content)
    }

    /// [`Shell::page`] with the three texts the shell writes itself in `language`: the `lang`
    /// attribute of `<html>`, the skip link and the `aria-label` of the navigation. The app's
    /// own texts (`app`, the labels, `content`) are written as they are given.
    pub fn page_in(&self, language: Language, title: &str, path: &str, content: Markup) -> Markup {
        html! {
            (DOCTYPE)
            html lang=(language.html_lang()) {
                head {
                    meta charset="utf-8";
                    meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover";
                    meta name="color-scheme" content="light dark";
                    meta name="theme-color" content="#2f6b3a" media="(prefers-color-scheme: light)";
                    meta name="theme-color" content="#141713" media="(prefers-color-scheme: dark)";
                    title { (title) " · " (self.app) }
                    link rel="stylesheet" href=(CSS_PATH);
                    (self.head)
                }
                body {
                    a class="skip-link" href="#main" { (language.skip_link()) }
                    div class="shell" {
                        nav class="nav" aria-label=(language.nav_label()) {
                            a class="brand" href="/" {
                                span { (self.app) }
                            }
                            ul class="nav-list" {
                                @for link in self.nav {
                                    li {
                                        a class="nav-link" href=(link.href)
                                            aria-current=[nav_active(link.href, path).then_some("page")] {
                                            span class="nav-label" { (link.label) }
                                        }
                                    }
                                }
                            }
                        }
                        main id="main" class="main" tabindex="-1" {
                            div class="page" { (content) }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    use super::*;
    use crate::test_util::{body_string, header_of};

    const SHELL: Shell = Shell {
        app: "Planner",
        nav: &[
            NavLink {
                href: "/",
                label: "Heute",
            },
            NavLink {
                href: "/aufgaben",
                label: "Aufgaben",
            },
        ],
        head: PreEscaped(r#"<link rel="manifest" href="/manifest.webmanifest">"#),
    };

    /// The custom properties a block of CSS declares (`--name: value;`).
    fn declared(css: &str) -> BTreeSet<&str> {
        css.lines()
            .map(str::trim)
            .filter(|line| line.starts_with("--"))
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name)
            .collect()
    }

    /// The stylesheet split into the dark `:root` block and everything else.
    fn light_and_dark() -> (String, String) {
        let (mut light, mut dark, mut in_dark) = (String::new(), String::new(), false);
        for line in CSS.lines() {
            if line.starts_with("@media (prefers-color-scheme: dark)") {
                in_dark = true;
            }
            let part = if in_dark { &mut dark } else { &mut light };
            part.push_str(line);
            part.push('\n');
            if in_dark && line == "}" {
                in_dark = false;
            }
        }
        (light, dark)
    }

    #[test]
    fn every_token_the_stylesheet_uses_is_declared() {
        let (light, dark) = light_and_dark();
        let tokens = declared(&light);
        assert!(tokens.contains("--primary") && tokens.contains("--bg"));
        let used: BTreeSet<&str> = CSS
            .split("var(")
            .skip(1)
            .filter_map(|rest| rest.split([')', ',']).next())
            .map(str::trim)
            .collect();
        assert!(used.len() > 10, "{used:?}");
        let missing: Vec<_> = used.difference(&tokens).collect();
        assert!(missing.is_empty(), "used but not declared: {missing:?}");
        let unused: Vec<_> = tokens.difference(&used).collect();
        assert!(unused.is_empty(), "declared but never used: {unused:?}");
        // Dark changes colours only: every token it sets exists in light.
        let dark_tokens = declared(&dark);
        assert!(dark_tokens.len() > 10, "{dark_tokens:?}");
        let unknown: Vec<_> = dark_tokens.difference(&tokens).collect();
        assert!(unknown.is_empty(), "dark-only tokens: {unknown:?}");
    }

    #[test]
    fn the_stylesheet_needs_nothing_from_outside() {
        for forbidden in ["url(", "@import", "@font-face", "http:", "https:", "//"] {
            assert!(!CSS.contains(forbidden), "{forbidden}");
        }
        // The page's theme-color metas are the light primary and the dark background.
        let (light, dark) = light_and_dark();
        assert!(light.contains("--primary: #2f6b3a;"));
        assert!(dark.contains("--bg: #141713;"));
    }

    /// The body of the top-level rule `selector { ... }` (one that starts a line).
    fn rule(selector: &str) -> &'static str {
        let start = CSS
            .find(&format!("\n{selector} {{"))
            .unwrap_or_else(|| panic!("no rule for {selector}"));
        let rest = &CSS[start..];
        &rest[..rest.find('}').expect("an unclosed rule")]
    }

    #[test]
    fn a_long_word_breaks_inside_the_page() {
        // Everything in <main> inherits the break rule, so no text can widen the page ...
        assert!(rule(".main").contains("overflow-wrap: anywhere;"));
        // ... and a label next to a long value is not squeezed apart by it: the pair wraps onto
        // two lines, and the value may shrink below its longest word.
        assert!(rule(".kv > div").contains("flex-wrap: wrap;"));
        assert!(rule(".kv dd").contains("min-width: 0;"));
        // The navigation is outside <main>: its labels are not affected.
        assert!(!rule(".nav-link").contains("overflow-wrap"));
    }

    async fn get_css(if_none_match: Option<&str>) -> Response {
        let mut request = Request::builder().uri(CSS_PATH);
        if let Some(tag) = if_none_match {
            request = request.header("if-none-match", tag);
        }
        router::<()>()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_stylesheet_is_served_and_revalidated() {
        let response = get_css(None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header_of(&response, "content-type"),
            "text/css; charset=utf-8"
        );
        assert_eq!(header_of(&response, "cache-control"), "no-cache");
        let etag = header_of(&response, "etag").to_owned();
        assert!(
            etag.starts_with('"') && etag.ends_with('"') && etag.len() == 18,
            "{etag}"
        );
        assert_eq!(body_string(response).await, CSS);

        for tag in [etag.clone(), format!("W/{etag}"), format!("\"x\", {etag}")] {
            let response = get_css(Some(&tag)).await;
            assert_eq!(response.status(), StatusCode::NOT_MODIFIED, "{tag}");
            assert_eq!(header_of(&response, "etag"), etag);
            assert_eq!(body_string(response).await, "");
        }
        let response = get_css(Some("\"another-version\"")).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn the_etag_follows_the_content() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(fnv1a(b"--bg: #fff"), fnv1a(b"--bg: #000"));
    }

    #[test]
    fn the_page_is_the_shell_around_the_content() {
        let page = SHELL
            .page("Aufgaben", "/aufgaben", html! { h1 { "Aufgaben" } })
            .into_string();
        assert!(
            page.starts_with("<!DOCTYPE html><html lang=\"de\"><head>"),
            "{page}"
        );
        for part in [
            r#"<meta charset="utf-8">"#,
            r#"<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">"#,
            r#"<meta name="color-scheme" content="light dark">"#,
            r##"<meta name="theme-color" content="#2f6b3a" media="(prefers-color-scheme: light)">"##,
            r##"<meta name="theme-color" content="#141713" media="(prefers-color-scheme: dark)">"##,
            "<title>Aufgaben · Planner</title>",
            r#"<link rel="stylesheet" href="/aba/aba-theme.css">"#,
            r#"<link rel="manifest" href="/manifest.webmanifest"></head>"#,
            r##"<body><a class="skip-link" href="#main">Zum Inhalt springen</a>"##,
            r#"<nav class="nav" aria-label="Hauptnavigation"><a class="brand" href="/"><span>Planner</span></a>"#,
            r#"<li><a class="nav-link" href="/"><span class="nav-label">Heute</span></a></li>"#,
            r#"<a class="nav-link" href="/aufgaben" aria-current="page"><span class="nav-label">Aufgaben</span></a>"#,
            r#"<main id="main" class="main" tabindex="-1"><div class="page"><h1>Aufgaben</h1></div></main>"#,
        ] {
            assert!(page.contains(part), "missing {part}\nin {page}");
        }
        assert!(page.ends_with("</div></body></html>"), "{page}");
        // No inline script or style: the page must pass the Content-Security-Policy.
        for forbidden in ["<script", "<style", "style=", "onclick"] {
            assert!(!page.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn the_shell_speaks_the_language_it_is_given() {
        let content = || html! { h1 { "Aufgaben" } };
        let german = SHELL.page("Aufgaben", "/aufgaben", content()).into_string();
        let english = SHELL
            .page_in(Language::English, "Aufgaben", "/aufgaben", content())
            .into_string();

        // Without a language the page is what it always was: German.
        let explicit = SHELL
            .page_in(Language::German, "Aufgaben", "/aufgaben", content())
            .into_string();
        assert_eq!(german, explicit);
        assert!(german.starts_with("<!DOCTYPE html><html lang=\"de\"><head>"));

        assert!(
            english.starts_with("<!DOCTYPE html><html lang=\"en\"><head>"),
            "{english}"
        );
        assert!(english.contains(r##"<a class="skip-link" href="#main">Skip to content</a>"##));
        assert!(english.contains(
            r#"<nav class="nav" aria-label="Main navigation"><a class="brand" href="/">"#
        ));
        for german_text in ["Zum Inhalt springen", "Hauptnavigation", "lang=\"de\""] {
            assert!(!english.contains(german_text), "{german_text}");
        }

        // Exactly those three places differ: the German page with them swapped is the English one.
        let swapped = german
            .replace("lang=\"de\"", "lang=\"en\"")
            .replace("Zum Inhalt springen", "Skip to content")
            .replace("Hauptnavigation", "Main navigation");
        assert_eq!(swapped, english);

        // The English shell is as free of inline script and style as the German one.
        for forbidden in ["<script", "<style", "style=", "onclick"] {
            assert!(!english.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn the_apps_own_texts_are_not_translated() {
        // A title or a label that happens to be a German shell text stays as the app wrote it.
        let shell = Shell {
            app: "Hauptnavigation",
            nav: &[NavLink {
                href: "/",
                label: "Zum Inhalt springen",
            }],
            head: PreEscaped(""),
        };
        let page = shell
            .page_in(
                Language::English,
                "Zum Inhalt springen",
                "/",
                html! { p { "Hauptnavigation" } },
            )
            .into_string();
        assert!(page.contains("<title>Zum Inhalt springen · Hauptnavigation</title>"));
        assert!(page.contains(r#"<span class="nav-label">Zum Inhalt springen</span>"#));
        assert!(page.contains("<p>Hauptnavigation</p>"));
        assert!(
            page.contains(">Skip to content</a>")
                && page.contains(r#"aria-label="Main navigation""#)
        );
    }

    #[test]
    fn texts_are_escaped_and_only_the_head_extra_is_raw() {
        let shell = Shell {
            app: "A&B",
            nav: &[NavLink {
                href: "/a\"b",
                label: "<i>x</i>",
            }],
            head: PreEscaped(""),
        };
        let page = shell
            .page("<b>Titel</b>", "/", html! { p { "1 < 2" } })
            .into_string();
        assert!(page.contains("<title>&lt;b&gt;Titel&lt;/b&gt; · A&amp;B</title>"));
        assert!(page.contains(r#"href="/a&quot;b""#));
        assert!(page.contains("&lt;i&gt;x&lt;/i&gt;"));
        assert!(page.contains("<p>1 &lt; 2</p>"));
        assert!(!page.contains("<b>") && !page.contains("<i>"));
    }

    #[test]
    fn one_navigation_entry_is_current() {
        assert!(nav_active("/", "/"));
        assert!(!nav_active("/", "/aufgaben"));
        assert!(nav_active("/aufgaben", "/aufgaben"));
        assert!(nav_active("/aufgaben", "/aufgaben/42"));
        assert!(!nav_active("/aufgaben", "/aufgabenliste"));
        assert!(!nav_active("/aufgaben", "/"));

        let current = |path: &str| {
            SHELL
                .page("x", path, html! {})
                .into_string()
                .matches("aria-current=\"page\"")
                .count()
        };
        assert_eq!(current("/"), 1);
        assert_eq!(current("/aufgaben/42"), 1);
        assert_eq!(current("/anderswo"), 0);
    }

    #[test]
    fn every_class_of_the_shell_is_styled() {
        let page = SHELL.page("x", "/", html! {}).into_string();
        let classes: BTreeSet<&str> = page
            .split("class=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .flat_map(str::split_whitespace)
            .collect();
        assert!(
            classes.contains("shell") && classes.contains("nav-link"),
            "{classes:?}"
        );
        for class in classes {
            // The label needs no rule of its own; everything else has one.
            if class != "nav-label" {
                assert!(
                    CSS.contains(&format!(".{class} {{")) || CSS.contains(&format!(".{class},")),
                    "{class}"
                );
            }
        }
    }
}
