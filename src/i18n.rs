//! Interface language: English or Russian, taken from the language of the
//! Windows interface unless chosen in the About window.
//!
//! Strings stay next to the code that shows them, both languages together:
//! `tr!("Open", "Открыть")` gives the one of the current language (and
//! evaluates only that one, so `format!` arms cost nothing extra).

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lang {
    #[default]
    En,
    Ru,
}

/// The language as kept in the settings: a fixed one, or the system's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LangChoice {
    #[default]
    System,
    En,
    Ru,
}

impl LangChoice {
    pub const ALL: [LangChoice; 3] = [LangChoice::System, LangChoice::En, LangChoice::Ru];

    pub fn resolve(self) -> Lang {
        match self {
            LangChoice::System => system_lang(),
            LangChoice::En => Lang::En,
            LangChoice::Ru => Lang::Ru,
        }
    }

    /// As listed in the About window: the languages in their own language,
    /// so that they can be found whatever the interface shows.
    pub fn label(self) -> String {
        let own = |lang| match lang {
            Lang::En => "English",
            Lang::Ru => "Русский",
        };
        match self {
            LangChoice::System => {
                let own = own(system_lang());
                crate::tr!(
                    format!("As in Windows ({own})"),
                    format!("Как в Windows ({own})")
                )
            }
            LangChoice::En | LangChoice::Ru => own(self.resolve()).into(),
        }
    }
}

/// An unknown name (a language of a later version) reads as `System`
/// rather than making the whole settings file unreadable.
impl<'de> Deserialize<'de> for LangChoice {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(d)?.as_str() {
            "en" => LangChoice::En,
            "ru" => LangChoice::Ru,
            _ => LangChoice::System,
        })
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

/// The language the interface is shown in now.
pub fn lang() -> Lang {
    match LANG.load(Relaxed) {
        1 => Lang::Ru,
        _ => Lang::En,
    }
}

pub fn set_lang(lang: Lang) {
    LANG.store(lang as u8, Relaxed);
}

/// Russian when Windows shows its interface in Russian, else English.
#[cfg(windows)]
pub fn system_lang() -> Lang {
    const LANG_RUSSIAN: u16 = 0x19;
    let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    if id & 0x3FF == LANG_RUSSIAN {
        Lang::Ru
    } else {
        Lang::En
    }
}

/// Russian when the locale (`LC_ALL`, `LC_MESSAGES` or `LANG`) is, else
/// English.
#[cfg(not(windows))]
pub fn system_lang() -> Lang {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|v| !v.is_empty())
        .unwrap_or_default();
    if locale.starts_with("ru") {
        Lang::Ru
    } else {
        Lang::En
    }
}

/// The English or the Russian expression, by the current language.
#[macro_export]
macro_rules! tr {
    ($en:expr, $ru:expr $(,)?) => {
        match $crate::i18n::lang() {
            $crate::i18n::Lang::En => $en,
            $crate::i18n::Lang::Ru => $ru,
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_is_stored_by_name() {
        for (choice, name) in [
            (LangChoice::System, "\"system\""),
            (LangChoice::En, "\"en\""),
            (LangChoice::Ru, "\"ru\""),
        ] {
            assert_eq!(serde_json::to_string(&choice).unwrap(), name);
            assert_eq!(serde_json::from_str::<LangChoice>(name).unwrap(), choice);
        }
        assert_eq!(
            serde_json::from_str::<LangChoice>("\"de\"").unwrap(),
            LangChoice::System
        );
        assert_eq!(LangChoice::Ru.label(), "Русский");
    }
}
