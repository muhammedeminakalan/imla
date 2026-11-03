//! Interface language: Turkish when the session locale is Turkish, English otherwise.

use std::sync::OnceLock;

/// True when LC_ALL / LC_MESSAGES / LANG (first one set) names Turkish.
pub fn is_tr() -> bool {
    static TR: OnceLock<bool> = OnceLock::new();
    *TR.get_or_init(|| {
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find(|v| !v.is_empty())
            .is_some_and(|v| v.to_lowercase().starts_with("tr"))
    })
}

/// Picks the Turkish or the English text.
pub fn t(tr: &'static str, en: &'static str) -> &'static str {
    if is_tr() { tr } else { en }
}

/// `format!` with a Turkish and an English format string.
#[macro_export]
macro_rules! tf {
    ($tr:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::is_tr() { format!($tr $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}
