//! The language of the six texts aba-kit writes itself: [`Language`].

/// The language of the six texts aba-kit writes itself. Everything else on a page or in an error
/// is the app's; this changes exactly these:
///
/// | Where | German (the default) | English |
/// |---|---|---|
/// | `lang` attribute of `<html>` | `de` | `en` |
/// | the skip link of the page shell | Zum Inhalt springen | Skip to content |
/// | `aria-label` of the navigation | Hauptnavigation | Main navigation |
/// | 401 of [`AbaUser`](crate::AbaUser): `message` of the JSON error | Nicht angemeldet | Not signed in |
/// | 400 of [`AbaUser`](crate::AbaUser): `message` of the JSON error | Ungültiger Benutzer im X-Aba-User-Header | Invalid user in the X-Aba-User header |
/// | 403 of the cross-site guard: `message` of the JSON error | Änderungen nur aus der App selbst | Changes only from the app itself |
///
/// German is the default, so an app that sets nothing is unchanged. An app sets the language in
/// the three places that write these texts, with one constant:
///
/// ```
/// use aba_kit::{
///     Identity, Language, http,
///     theme::{NavLink, Shell},
/// };
/// use axum::{Router, middleware};
/// use maud::{PreEscaped, html};
///
/// const LANGUAGE: Language = Language::English;
/// const SHELL: Shell = Shell {
///     app: "Planner",
///     nav: &[NavLink { href: "/", label: "Today" }],
///     head: PreEscaped(""),
/// };
///
/// // The page shell: lang, skip link and navigation label.
/// let page = SHELL.page_in(LANGUAGE, "Today", "/", html! { h1 { "Today" } });
/// assert!(page.into_string().contains(r#"<html lang="en">"#));
///
/// // The identity's 401 and 400 (the identity lives in the app's state; an app builds it with
/// // `Identity::from_env()?` or `Identity::new(..)` and adds the language).
/// let identity = Identity::default().with_language(LANGUAGE);
///
/// // The cross-site guard's 403 (it still guards every path).
/// let app: Router = Router::new().layer(middleware::from_fn_with_state(
///     LANGUAGE,
///     http::reject_cross_site_writes_in,
/// ));
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Language {
    /// What aba-kit has always written.
    #[default]
    German,
    /// The same six texts in English.
    English,
}

impl Language {
    /// The value of `<html lang>`.
    pub(crate) const fn html_lang(self) -> &'static str {
        match self {
            Language::German => "de",
            Language::English => "en",
        }
    }

    /// The text of the skip link in the page shell.
    pub(crate) const fn skip_link(self) -> &'static str {
        match self {
            Language::German => "Zum Inhalt springen",
            Language::English => "Skip to content",
        }
    }

    /// The `aria-label` of the navigation in the page shell.
    pub(crate) const fn nav_label(self) -> &'static str {
        match self {
            Language::German => "Hauptnavigation",
            Language::English => "Main navigation",
        }
    }

    /// The message of the 401 for a request without a user.
    pub(crate) const fn not_signed_in(self) -> &'static str {
        match self {
            Language::German => "Nicht angemeldet",
            Language::English => "Not signed in",
        }
    }

    /// The message of the 400 for a malformed `X-Aba-User`.
    pub(crate) const fn invalid_user(self) -> &'static str {
        match self {
            Language::German => "Ungültiger Benutzer im X-Aba-User-Header",
            Language::English => "Invalid user in the X-Aba-User header",
        }
    }

    /// The message of the 403 for a write from another site.
    pub(crate) const fn cross_site_write(self) -> &'static str {
        match self {
            Language::German => "Änderungen nur aus der App selbst",
            Language::English => "Changes only from the app itself",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The six texts of one language, in the order of the table above.
    fn texts(language: Language) -> [&'static str; 6] {
        [
            language.html_lang(),
            language.skip_link(),
            language.nav_label(),
            language.not_signed_in(),
            language.invalid_user(),
            language.cross_site_write(),
        ]
    }

    #[test]
    fn german_is_the_default_and_says_what_it_always_said() {
        assert_eq!(Language::default(), Language::German);
        assert_eq!(
            texts(Language::German),
            [
                "de",
                "Zum Inhalt springen",
                "Hauptnavigation",
                "Nicht angemeldet",
                "Ungültiger Benutzer im X-Aba-User-Header",
                "Änderungen nur aus der App selbst",
            ]
        );
    }

    #[test]
    fn english_says_the_same_six_things() {
        assert_eq!(
            texts(Language::English),
            [
                "en",
                "Skip to content",
                "Main navigation",
                "Not signed in",
                "Invalid user in the X-Aba-User header",
                "Changes only from the app itself",
            ]
        );
        // Every text differs between the languages: none was left German by mistake.
        for (german, english) in texts(Language::German)
            .into_iter()
            .zip(texts(Language::English))
        {
            assert_ne!(german, english);
        }
    }
}
