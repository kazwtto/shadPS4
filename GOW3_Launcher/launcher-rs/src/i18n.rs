//! Idiomas do launcher.
//!
//! O texto em português é a chave do catálogo (`assets/i18n.json`) e o manifesto
//! `assets/i18n_keys.json` fixa a ordem usada pela interface (`L.s[i]`). O idioma
//! do launcher segue o idioma do console por padrão, mas pode ser escolhido.

use std::sync::OnceLock;

use slint::{ModelRc, SharedString, VecModel};

use crate::AppWindow;

/// Idiomas disponíveis, na ordem mostrada no seletor (depois de "seguir o console").
pub const LANGUAGES: [(&str, Lang); 5] = [
    ("Português (Brasil)", Lang::Pt),
    ("English", Lang::En),
    ("Español", Lang::Es),
    ("Français", Lang::Fr),
    ("Deutsch", Lang::De),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Pt,
    En,
    Es,
    Fr,
    De,
}

impl Lang {
    /// Posição no seletor: 0 é "seguir o console".
    pub fn choice_index(self) -> i32 {
        LANGUAGES
            .iter()
            .position(|(_, lang)| *lang == self)
            .map(|index| index as i32 + 1)
            .unwrap_or(0)
    }

    pub fn from_choice(choice: i32) -> Option<Self> {
        if choice <= 0 {
            return None;
        }
        LANGUAGES
            .get(choice as usize - 1)
            .map(|(_, lang)| *lang)
    }

    /// Coluna do catálogo: inglês, espanhol, francês e alemão.
    fn slot(self) -> Option<usize> {
        match self {
            Lang::Pt => None,
            Lang::En => Some(0),
            Lang::Es => Some(1),
            Lang::Fr => Some(2),
            Lang::De => Some(3),
        }
    }

    /// Idioma do launcher a partir do código `General.console_language`.
    pub fn from_console(code: i64) -> Self {
        match code {
            // Português (Brasil), Português (Portugal)
            17 | 7 => Lang::Pt,
            // Inglês (Estados Unidos), Inglês (Reino Unido)
            1 | 18 => Lang::En,
            // Espanhol (Espanha), Espanhol (América Latina)
            3 | 20 => Lang::Es,
            // Francês (França), Francês (Canadá)
            2 | 22 => Lang::Fr,
            // Alemão
            4 => Lang::De,
            // Sem config de console (ou idioma sem catálogo próprio): o launcher
            // mostra o idioma base dele, português, e não um inglês forçado.
            _ => Lang::Pt,
        }
    }

}

/// Idioma efetivo: escolha explícita ou o idioma do console.
pub fn resolve(choice: i32, console_language: i64) -> Lang {
    Lang::from_choice(choice).unwrap_or_else(|| Lang::from_console(console_language))
}

fn keys() -> &'static Vec<String> {
    static KEYS: OnceLock<Vec<String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        serde_json::from_str(include_str!("../assets/i18n_keys.json")).unwrap_or_default()
    })
}

fn catalog() -> &'static serde_json::Map<String, serde_json::Value> {
    static CATALOG: OnceLock<serde_json::Map<String, serde_json::Value>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str::<serde_json::Value>(include_str!("../assets/i18n.json"))
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default()
    })
}

/// Tradução de uma chave; sem entrada no catálogo, fica o português.
pub fn translate(lang: Lang, key: &str) -> String {
    let Some(slot) = lang.slot() else {
        return key.to_string();
    };
    catalog()
        .get(key)
        .and_then(|value| value.as_array())
        .and_then(|values| values.get(slot))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| key.to_string())
}

/// Sequência de textos do idioma na ordem do manifesto, para `L.s`.
pub fn strings(lang: Lang) -> Vec<SharedString> {
    keys()
        .iter()
        .map(|key| SharedString::from(translate(lang, key)))
        .collect()
}

pub fn apply(ui: &AppWindow, lang: Lang) {
    ui.set_tr_s(ModelRc::new(VecModel::from(strings(lang))));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_and_the_catalog_are_aligned() {
        let keys = keys();
        assert!(keys.len() > 100, "manifesto com {} chaves", keys.len());
        let catalog = catalog();
        for key in keys {
            assert!(
                catalog.contains_key(key),
                "chave sem tradução no catálogo: {key}"
            );
            let slots = catalog.get(key).and_then(|value| value.as_array()).unwrap();
            assert_eq!(slots.len(), 4, "quatro idiomas para {key}");
            for slot in slots {
                assert!(
                    slot.as_str().map(|text| !text.trim().is_empty()).unwrap_or(false),
                    "tradução vazia para {key}"
                );
            }
        }
        assert!(catalog.len() >= keys.len());
    }

    #[test]
    fn every_language_returns_the_full_sequence() {
        for (_, lang) in LANGUAGES {
            let texts = strings(lang);
            assert_eq!(texts.len(), keys().len());
            assert!(texts.iter().all(|text| !text.is_empty()));
        }
    }

    #[test]
    fn console_languages_map_to_launcher_languages() {
        assert_eq!(Lang::from_console(17), Lang::Pt);
        assert_eq!(Lang::from_console(7), Lang::Pt);
        assert_eq!(Lang::from_console(1), Lang::En);
        assert_eq!(Lang::from_console(18), Lang::En);
        assert_eq!(Lang::from_console(3), Lang::Es);
        assert_eq!(Lang::from_console(20), Lang::Es);
        assert_eq!(Lang::from_console(2), Lang::Fr);
        assert_eq!(Lang::from_console(22), Lang::Fr);
        assert_eq!(Lang::from_console(4), Lang::De);
        assert_eq!(
            Lang::from_console(0),
            Lang::Pt,
            "sem config de console o launcher abre em português, não em inglês"
        );
        assert_eq!(Lang::from_console(99), Lang::Pt, "código sem catálogo cai no idioma base");
        assert_eq!(
            Lang::from_console(-1),
            Lang::Pt,
            "sem arquivo de configuração o idioma é o base"
        );
    }

    #[test]
    fn the_launcher_language_follows_the_console_unless_chosen() {
        assert_eq!(resolve(0, 3), Lang::Es);
        assert_eq!(resolve(0, 17), Lang::Pt);
        assert_eq!(resolve(2, 17), Lang::En, "a escolha explícita vence");
        assert_eq!(resolve(5, 1), Lang::De);
        assert_eq!(resolve(9, 1), Lang::En, "índice fora da lista volta ao console");
        assert_eq!(Lang::from_choice(1), Some(Lang::Pt));
        assert_eq!(Lang::chooser_position(Lang::Pt), 1);
    }

    impl Lang {
        fn chooser_position(self) -> i32 {
            self.choice_index()
        }
    }

    #[test]
    fn unknown_keys_fall_back_to_portuguese() {
        assert_eq!(translate(Lang::En, "chave inexistente"), "chave inexistente");
        let key = &keys()[0];
        assert_eq!(translate(Lang::Pt, key), *key);
    }
}
