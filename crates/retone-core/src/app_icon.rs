//! The app icon picked in Settings (`APP_ICON` in `userData/.env`). The bundle
//! always ships [`AppIcon::Mono`]; the others replace the Dock and menu-bar
//! icons while ReTone runs, since rewriting the signed bundle would break its
//! signature and the Accessibility grant.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppIcon {
    /// Off-white tile, black R and three tone dots: the bundle icon.
    #[default]
    Mono,
    /// White tile with a coral-to-violet R.
    Light,
    /// Night-blue mesh gradient behind a white R.
    Aurora,
    /// Dark tile with a multicolour refresh ring around R.
    Ring,
}

impl AppIcon {
    pub const ALL: [AppIcon; 4] = [
        AppIcon::Mono,
        AppIcon::Light,
        AppIcon::Aurora,
        AppIcon::Ring,
    ];

    /// The id the renderer and `.env` use.
    pub fn id(self) -> &'static str {
        match self {
            AppIcon::Mono => "mono",
            AppIcon::Light => "light",
            AppIcon::Aurora => "aurora",
            AppIcon::Ring => "ring",
        }
    }

    /// Parses an id, ignoring surrounding whitespace and case.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        Self::ALL
            .into_iter()
            .find(|icon| icon.id().eq_ignore_ascii_case(value))
    }

    /// The saved choice, or the default when nothing (or an unknown id) is saved.
    pub fn from_saved(value: Option<&str>) -> Self {
        value.and_then(Self::parse).unwrap_or_default()
    }

    /// Whether this is the icon the `.app` bundle already carries.
    pub fn is_bundle_icon(self) -> bool {
        self == AppIcon::Mono
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for icon in AppIcon::ALL {
            assert_eq!(AppIcon::parse(icon.id()), Some(icon));
        }
    }

    #[test]
    fn parse_ignores_case_and_whitespace_and_rejects_unknown_ids() {
        assert_eq!(AppIcon::parse(" Aurora\n"), Some(AppIcon::Aurora));
        assert_eq!(AppIcon::parse("RING"), Some(AppIcon::Ring));
        assert_eq!(AppIcon::parse("green"), None);
        assert_eq!(AppIcon::parse(""), None);
    }

    #[test]
    fn unsaved_or_unknown_choices_fall_back_to_the_bundle_icon() {
        assert_eq!(AppIcon::from_saved(None), AppIcon::Mono);
        assert_eq!(AppIcon::from_saved(Some("retired")), AppIcon::Mono);
        assert_eq!(AppIcon::from_saved(Some("light")), AppIcon::Light);
        assert!(AppIcon::default().is_bundle_icon());
        assert!(!AppIcon::Ring.is_bundle_icon());
    }
}
