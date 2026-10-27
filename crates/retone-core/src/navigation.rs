//! Navigation guard and external-link rules (including the `open-external`
//! scheme allowlist).

use url::Url;

/// A navigation is the window's own page (reload, query/hash change) when the
/// scheme, host, port and path match the URL the window was loaded with.
/// `file:` URLs have an opaque origin, so their path carries the comparison.
pub fn is_allowed_app_navigation(url: &str, app_url: Option<&str>) -> bool {
    if url.starts_with("devtools://") {
        return true;
    }
    let Some(app_url) = app_url else {
        return false;
    };
    let (Ok(candidate), Ok(app)) = (Url::parse(url), Url::parse(app_url)) else {
        return false;
    };
    candidate.scheme() == app.scheme()
        && candidate.host_str() == app.host_str()
        && candidate.port_or_known_default() == app.port_or_known_default()
        && page_path(candidate.path()) == page_path(app.path())
}

/// `/` and `/index.html` are the same page: Tauri loads `WebviewUrl::App("index.html")`
/// as the bare root URL. On the custom protocol that root is `tauri://localhost`,
/// whose path is empty (a non-special scheme has no implied `/`).
fn page_path(path: &str) -> &str {
    match path.strip_suffix("index.html").unwrap_or(path) {
        "" => "/",
        other => other,
    }
}

pub fn is_external_browser_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Only web and mail links are handed to the OS, never a local path.
pub fn is_external_open_url(url: &str) -> bool {
    is_external_browser_url(url) || url.starts_with("mailto:")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalUrlError {
    Invalid(String),
    BlockedScheme(String),
}

impl std::fmt::Display for ExternalUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "Invalid URL: {reason}"),
            Self::BlockedScheme(scheme) => write!(f, "Blocked URL scheme: {scheme}:"),
        }
    }
}

impl std::error::Error for ExternalUrlError {}

/// What the `open_external` command accepts: a parseable http(s) or mailto URL.
pub fn validate_external_url(url: &str) -> Result<(), ExternalUrlError> {
    let parsed = Url::parse(url).map_err(|e| ExternalUrlError::Invalid(e.to_string()))?;
    match parsed.scheme() {
        "http" | "https" | "mailto" => Ok(()),
        other => Err(ExternalUrlError::BlockedScheme(other.to_string())),
    }
}

/// Hands a URL to the OS. The app implements it with `tauri-plugin-opener`.
pub trait UrlOpener {
    fn open(&self, url: &str) -> Result<(), String>;
}

/// Parses first so anything that is not a URL is rejected before the OS sees
/// it; the URL itself is passed on untouched.
pub fn open_external_url(opener: &dyn UrlOpener, url: &str) -> Result<(), String> {
    Url::parse(url).map_err(|e| ExternalUrlError::Invalid(e.to_string()).to_string())?;
    opener.open(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const PACKAGED_APP_URL: &str =
        "file:///Applications/ReTone.app/Contents/Resources/dist/index.html";

    #[test]
    fn packaged_app_reload_is_allowed_regardless_of_query_or_hash() {
        let app = Some(PACKAGED_APP_URL);
        assert!(is_allowed_app_navigation(
            &format!("{PACKAGED_APP_URL}?panel=true"),
            app
        ));
        assert!(is_allowed_app_navigation(PACKAGED_APP_URL, app));
        assert!(is_allowed_app_navigation(
            &format!("{PACKAGED_APP_URL}?panel=true#settings"),
            app
        ));
    }

    #[test]
    fn tauri_app_url_reload_is_allowed() {
        let app = Some("tauri://localhost/index.html?tone-rewrite-picker=true");
        assert!(is_allowed_app_navigation(
            "tauri://localhost/index.html",
            app
        ));
        assert!(!is_allowed_app_navigation(
            "tauri://localhost/other.html",
            app
        ));
    }

    // A packaged build's first load is the bare `tauri://localhost`; blocking
    // it left the settings window blank.
    #[test]
    fn the_bare_custom_protocol_root_is_the_app_page() {
        let app = Some("tauri://localhost/index.html");
        assert!(is_allowed_app_navigation("tauri://localhost", app));
        assert!(is_allowed_app_navigation("tauri://localhost/", app));
        assert!(is_allowed_app_navigation(
            "tauri://localhost?tone-rewrite-picker=true",
            Some("tauri://localhost/index.html?tone-rewrite-picker=true")
        ));
        assert!(!is_allowed_app_navigation("tauri://localhost/other", app));
        assert!(!is_allowed_app_navigation("tauri://evil", app));
    }

    #[test]
    fn root_and_index_html_are_the_same_page() {
        let app = Some("http://127.0.0.1:5183/index.html");
        assert!(is_allowed_app_navigation("http://127.0.0.1:5183/", app));
        assert!(is_allowed_app_navigation(
            "http://127.0.0.1:5183/?tone-rewrite-picker=true",
            app
        ));
        assert!(!is_allowed_app_navigation(
            "http://127.0.0.1:5183/other/",
            app
        ));
    }

    #[test]
    fn windows_style_file_urls_compare_on_the_encoded_pathname() {
        let win = "file:///C:/Program%20Files/ReTone/resources/dist/index.html";
        assert!(is_allowed_app_navigation(
            &format!("{win}?panel=true"),
            Some(win)
        ));
    }

    #[test]
    fn foreign_file_urls_are_blocked() {
        let app = Some(PACKAGED_APP_URL);
        assert!(!is_allowed_app_navigation("file:///etc/passwd", app));
        assert!(!is_allowed_app_navigation(
            "file:///Users/test/secret.txt",
            app
        ));
    }

    #[test]
    fn remote_and_unparseable_urls_are_blocked() {
        let app = Some(PACKAGED_APP_URL);
        assert!(!is_allowed_app_navigation("https://example.com/", app));
        assert!(!is_allowed_app_navigation("http://localhost:5183/", app));
        assert!(!is_allowed_app_navigation("not a url", app));
    }

    #[test]
    fn a_missing_app_url_blocks_everything_except_devtools() {
        assert!(!is_allowed_app_navigation("file:///etc/passwd", None));
        assert!(!is_allowed_app_navigation("https://example.com/", None));
        assert!(is_allowed_app_navigation(
            "devtools://devtools/bundled/inspector.html",
            None
        ));
    }

    #[test]
    fn development_route_and_devtools_stay_available() {
        let app = Some("http://localhost:5183/?panel=true");
        assert!(is_allowed_app_navigation("http://localhost:5183/", app));
        assert!(is_allowed_app_navigation(
            "http://localhost:5183/?panel=true",
            app
        ));
        assert!(!is_allowed_app_navigation(
            "http://localhost:5183/other",
            app
        ));
        assert!(!is_allowed_app_navigation("http://localhost:5184/", app));
    }

    #[test]
    fn only_http_targets_go_to_the_external_browser() {
        assert!(is_external_browser_url("https://example.com/docs"));
        assert!(is_external_browser_url("http://example.com/"));
        assert!(!is_external_browser_url("file:///etc/passwd"));
        assert!(!is_external_browser_url("about:blank"));
        assert!(!is_external_browser_url(
            "devtools://devtools/bundled/inspector.html"
        ));
    }

    #[test]
    fn window_open_hands_the_os_only_web_and_mail_links() {
        assert!(is_external_open_url("https://example.com/docs"));
        assert!(is_external_open_url("http://example.com/"));
        assert!(is_external_open_url("mailto:someone@example.com"));
        assert!(!is_external_open_url("file:///C:/Windows/System32/cmd.exe"));
        assert!(!is_external_open_url(
            "file://attacker.example/share/run.exe"
        ));
        assert!(!is_external_open_url("javascript:alert(1)"));
        assert!(!is_external_open_url("about:blank"));
    }

    #[test]
    fn open_external_allows_only_web_and_mail_schemes() {
        assert!(validate_external_url("https://openrouter.ai/keys").is_ok());
        assert!(validate_external_url("mailto:a@b.c").is_ok());
        assert_eq!(
            validate_external_url("file:///etc/passwd"),
            Err(ExternalUrlError::BlockedScheme("file".into()))
        );
        assert!(matches!(
            validate_external_url("not a url"),
            Err(ExternalUrlError::Invalid(_))
        ));
    }

    #[derive(Default)]
    struct RecordingOpener {
        calls: RefCell<Vec<String>>,
    }

    impl UrlOpener for RecordingOpener {
        fn open(&self, url: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(url.to_string());
            Ok(())
        }
    }

    #[test]
    fn links_open_untouched() {
        let opener = RecordingOpener::default();
        open_external_url(&opener, "https://openrouter.ai/keys?ref=x#top").unwrap();
        open_external_url(&opener, "mailto:someone@example.com").unwrap();
        assert_eq!(
            *opener.calls.borrow(),
            vec![
                "https://openrouter.ai/keys?ref=x#top".to_string(),
                "mailto:someone@example.com".to_string()
            ]
        );
    }

    #[test]
    fn invalid_urls_reject_without_opening_anything() {
        let opener = RecordingOpener::default();
        assert!(open_external_url(&opener, "not a url").is_err());
        assert!(opener.calls.borrow().is_empty());
    }
}
