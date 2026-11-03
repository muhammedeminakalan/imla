//! User settings in ~/.config/imla/config.toml (owner-only: it holds the API key).
//!
//! Two engines: OpenRouter (an audio-capable model in the cloud) or a local whisper.cpp model
//! kept in ~/.cache/imla/models.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "dev.akalan.imla";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// An audio-capable model on OpenRouter.
    Openrouter,
    /// whisper.cpp on this machine, offline.
    Local,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct OpenRouter {
    /// Empty: taken from OPENROUTER_API_KEY.
    pub api_key: String,
    pub model: String,
}

impl Default for OpenRouter {
    fn default() -> Self {
        Self { api_key: String::new(), model: "openai/gpt-audio-mini".into() }
    }
}

impl OpenRouter {
    pub fn key(&self) -> String {
        if self.api_key.is_empty() { std::env::var("OPENROUTER_API_KEY").unwrap_or_default() } else { self.api_key.clone() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Local {
    /// One of `crate::local::MODELS`.
    pub model: String,
}

impl Default for Local {
    fn default() -> Self {
        Self { model: crate::local::DEFAULT_MODEL.into() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// "openrouter" or "local".
    #[serde(with = "engine_name")]
    pub provider: Engine,
    pub openrouter: OpenRouter,
    pub local: Local,
    /// Earlier layouts, read once and folded into `openrouter`.
    #[serde(skip_serializing)]
    pub(crate) api_key: String,
    #[serde(skip_serializing)]
    pub(crate) model: String,
    #[serde(skip_serializing)]
    pub(crate) providers: BTreeMap<String, toml::Value>,
    /// ISO-639-1 language of the speech, or "auto": written in whatever language is spoken.
    pub language: String,
    /// Words the model should spell exactly (names, jargon); sent as hints with every request.
    pub terms: Vec<String>,
    /// Preferred shortcut, in the XDG shortcuts syntax; KDE lets you change it when binding.
    pub shortcut: String,
    /// Keys that paste the transcript: "ctrl+v" or "ctrl+shift+v" (terminals).
    pub paste_keys: String,
    /// Put the previous clipboard text back after pasting.
    pub restore_clipboard: bool,
    /// Play the recording tones (start, stop, dropped).
    pub sound_effects: bool,
    /// Tone volume, 0 to 1.
    pub sound_volume: f32,
    /// Recording stops by itself after this many seconds.
    pub max_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: Engine::Openrouter,
            openrouter: OpenRouter::default(),
            local: Local::default(),
            api_key: String::new(),
            model: String::new(),
            providers: BTreeMap::new(),
            language: "auto".into(),
            terms: Vec::new(),
            shortcut: "CTRL+ALT+D".into(),
            paste_keys: "ctrl+v".into(),
            restore_clipboard: true,
            sound_effects: true,
            sound_volume: 0.5,
            max_seconds: 120,
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().expect("no config dir").join("imla")
}

pub fn state_dir() -> PathBuf {
    dirs::state_dir().expect("no state dir").join("imla")
}

/// Moves settings and state kept under the project's former name ("dictate") to the new one.
pub fn migrate_old_name() {
    let pairs = [
        (dirs::config_dir(), "config"),
        (dirs::state_dir(), "state"),
    ];
    for (base, what) in pairs {
        let Some(base) = base else { continue };
        let (old, new) = (base.join("dictate"), base.join("imla"));
        if old.is_dir() && !new.exists() {
            match fs::rename(&old, &new) {
                Ok(()) => log::info!("{what}: {} -> {}", old.display(), new.display()),
                Err(e) => log::warn!("{what}: {} taşınamadı: {e}", old.display()),
            }
        }
    }
}

pub fn path() -> PathBuf {
    config_dir().join("config.toml")
}

impl Config {
    pub fn load() -> Result<Self> {
        let p = path();
        let mut cfg: Self = match fs::read_to_string(&p) {
            Ok(s) => toml::from_str(&s).with_context(|| crate::tf!("{} okunamadı", "could not read {}", p.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e).with_context(|| crate::tf!("{} okunamadı", "could not read {}", p.display())),
        };
        cfg.migrate_legacy();
        Ok(cfg)
    }

    fn migrate_legacy(&mut self) {
        // 0.3.x: [providers.openrouter] with api_key/model
        if let Some(t) = self.providers.remove("openrouter") {
            if let Some(k) = t.get("api_key").and_then(|v| v.as_str()).filter(|k| !k.is_empty()) {
                self.openrouter.api_key = k.into();
            }
            if let Some(m) = t.get("model").and_then(|v| v.as_str()).filter(|m| !m.is_empty()) {
                self.openrouter.model = m.into();
            }
        }
        self.providers.clear();
        // before 0.3: top-level api_key/model
        if !self.api_key.is_empty() {
            self.openrouter.api_key = std::mem::take(&mut self.api_key);
        }
        if !self.model.is_empty() {
            self.openrouter.model = std::mem::take(&mut self.model);
        }
    }

    pub fn save(&self) -> Result<()> {
        let dir = config_dir();
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let p = path();
        // created owner-only from the start: the file holds the API key
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&p)?;
        f.write_all(toml::to_string_pretty(self)?.as_bytes())?;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
}

/// `provider` as a name; unknown names (from 0.3's other profiles) fall back to OpenRouter.
mod engine_name {
    use super::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(e: &Engine, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match e {
            Engine::Openrouter => "openrouter",
            Engine::Local => "local",
        })
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Engine, D::Error> {
        Ok(if String::deserialize(d)? == "local" { Engine::Local } else { Engine::Openrouter })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> Config {
        let mut cfg: Config = toml::from_str(text).unwrap();
        cfg.migrate_legacy();
        cfg
    }

    #[test]
    fn earlier_layouts_move_into_openrouter() {
        let old = load("api_key = \"sk-1\"\nmodel = \"openai/gpt-audio\"\n");
        assert_eq!((old.openrouter.api_key.as_str(), old.openrouter.model.as_str()), ("sk-1", "openai/gpt-audio"));
        let v03 = load("provider = \"openrouter\"\n[providers.openrouter]\nkind = \"chat-audio\"\napi_key = \"sk-2\"\nmodel = \"m\"\n");
        assert_eq!((v03.provider, v03.openrouter.api_key.as_str()), (Engine::Openrouter, "sk-2"));
        let saved = toml::to_string(&v03).unwrap();
        assert!(!saved.contains("providers") && saved.contains("[openrouter]"));
    }

    #[test]
    fn local_round_trips() {
        let cfg = load("provider = \"local\"\n[local]\nmodel = \"small\"\n");
        assert_eq!((cfg.provider, cfg.local.model.as_str()), (Engine::Local, "small"));
        let back: Config = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.provider, Engine::Local);
        assert_eq!(load("provider = \"groq\"\n").provider, Engine::Openrouter);
    }
}
