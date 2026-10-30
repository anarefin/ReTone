//! The Settings "Check for updates" result. The app asks GitHub for the latest
//! published release and only links to it: nothing is downloaded or installed.

use serde_json::Value;

/// GitHub's latest published release (drafts and prereleases are never returned).
pub const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/anarefin/retone/releases/latest";

/// The only page an update link may open. GitHub spells the repo `ReTone`, so
/// the comparison ignores case.
const RELEASE_PAGE_PREFIX: &str = "https://github.com/anarefin/retone/releases/";

#[derive(Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    UpToDate,
    Available { version: String, url: String },
}

/// Parses `1.2.3` or `v1.2.3`. Pre-release or build suffixes are rejected.
pub fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.trim();
    let value = value.strip_prefix('v').unwrap_or(value);
    let mut parts = value.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

fn is_release_page(url: &str) -> bool {
    url.len() > RELEASE_PAGE_PREFIX.len()
        && url.is_char_boundary(RELEASE_PAGE_PREFIX.len())
        && url[..RELEASE_PAGE_PREFIX.len()].eq_ignore_ascii_case(RELEASE_PAGE_PREFIX)
}

/// Compares the running version with a `releases/latest` response. A release
/// with a malformed tag or a link outside the repo's release pages is an error.
pub fn evaluate(current: &str, release: &Value) -> Result<UpdateStatus, &'static str> {
    let current = parse_version(current).ok_or("invalid current version")?;
    let tag = release["tag_name"].as_str().ok_or("missing tag_name")?;
    let latest = parse_version(tag).ok_or("invalid tag_name")?;
    if latest <= current {
        return Ok(UpdateStatus::UpToDate);
    }
    let url = release["html_url"].as_str().ok_or("missing html_url")?;
    if !is_release_page(url) {
        return Err("unexpected html_url");
    }
    Ok(UpdateStatus::Available {
        version: format!("{}.{}.{}", latest.0, latest.1, latest.2),
        url: url.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str, url: &str) -> Value {
        json!({ "tag_name": tag, "html_url": url })
    }

    const PAGE: &str = "https://github.com/anarefin/ReTone/releases/tag/v0.2.0";

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("v10.0.3"), Some((10, 0, 3)));
        assert_eq!(parse_version(" v1.2.3 "), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("1.2.3-beta.1"), None);
        assert_eq!(parse_version("latest"), None);
    }

    #[test]
    fn newer_release_is_available() {
        assert_eq!(
            evaluate("0.1.0", &release("v0.2.0", PAGE)),
            Ok(UpdateStatus::Available {
                version: "0.2.0".into(),
                url: PAGE.into()
            })
        );
    }

    #[test]
    fn equal_or_older_release_is_up_to_date() {
        assert_eq!(
            evaluate("0.2.0", &release("v0.2.0", PAGE)),
            Ok(UpdateStatus::UpToDate)
        );
        assert_eq!(
            evaluate("1.0.0", &release("v0.9.9", PAGE)),
            Ok(UpdateStatus::UpToDate)
        );
    }

    #[test]
    fn compares_numerically() {
        assert!(matches!(
            evaluate("0.9.0", &release("v0.10.0", PAGE)),
            Ok(UpdateStatus::Available { .. })
        ));
    }

    #[test]
    fn rejects_malformed_tags() {
        assert!(evaluate("0.1.0", &release("nightly", PAGE)).is_err());
        assert!(evaluate("0.1.0", &json!({ "html_url": PAGE })).is_err());
    }

    #[test]
    fn rejects_links_outside_the_release_pages() {
        for url in [
            "https://evil.example/anarefin/retone/releases/tag/v0.2.0",
            "https://github.com/someone/retone/releases/tag/v0.2.0",
            "http://github.com/anarefin/retone/releases/tag/v0.2.0",
            "https://github.com/anarefin/retone/releases/",
        ] {
            assert!(evaluate("0.1.0", &release("v0.2.0", url)).is_err(), "{url}");
        }
    }
}
