//! User settings in ~/.config/imla/config.toml (owner-only: it holds API keys).
//!
//! Speech goes to the active *provider* profile. Three kinds cover the market and local setups:
//! an audio-capable chat model (OpenAI-compatible `chat/completions` with `input_audio`), an
//! OpenAI-compatible `audio/transcriptions` endpoint (cloud or a local server), or any command
//! that reads a WAV file and prints the text.

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
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// OpenAI-compatible `chat/completions` with an `input_audio` part.
    ChatAudio,
    /// OpenAI-compatible `audio/transcriptions` (multipart upload).
    Transcriptions,
    /// A local command; `{wav}` is replaced by the clip's path, `{lang}` by the language.
    Command,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Provider {
    pub kind: Kind,
    /// API root, e.g. `https://openrouter.ai/api/v1`.
    pub base_url: String,
    /// Empty for keyless servers; falls back to the `key_env` environment variable.
    pub api_key: String,
    pub key_env: String,
    pub model: String,
    /// Only for `kind = "command"`.
    pub command: String,
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            kind: Kind::Transcriptions,
            base_url: String::new(),
            api_key: String::new(),
            key_env: String::new(),
            model: String::new(),
            command: String::new(),
        }
    }
}

impl Provider {
    pub fn needs_key(&self) -> bool {
        self.kind != Kind::Command && !self.key_env.is_empty()
    }

    /// The key from the config, else from its environment variable.
    pub fn key(&self) -> String {
        if self.api_key.is_empty() && !self.key_env.is_empty() {
            std::env::var(&self.key_env).unwrap_or_default()
        } else {
            self.api_key.clone()
        }
    }
}

/// Ready-made profiles: name, profile, one-line description.
pub fn presets() -> Vec<(&'static str, Provider, &'static str)> {
    let p = |kind, base_url: &str, key_env: &str, model: &str| Provider {
        kind,
        base_url: base_url.into(),
        key_env: key_env.into(),
        model: model.into(),
        ..Provider::default()
    };
    vec![
        ("openrouter", p(Kind::ChatAudio, "https://openrouter.ai/api/v1", "OPENROUTER_API_KEY", "openai/gpt-audio-mini"),
         "OpenRouter: many audio and speech models with one key"),
        ("openai", p(Kind::Transcriptions, "https://api.openai.com/v1", "OPENAI_API_KEY", "gpt-4o-mini-transcribe"),
         "OpenAI speech-to-text"),
        ("groq", p(Kind::Transcriptions, "https://api.groq.com/openai/v1", "GROQ_API_KEY", "whisper-large-v3-turbo"),
         "Groq: very fast hosted Whisper"),
        ("gemini", p(Kind::ChatAudio, "https://generativelanguage.googleapis.com/v1beta/openai", "GEMINI_API_KEY", "gemini-2.5-flash"),
         "Google Gemini (OpenAI-compatible endpoint)"),
        ("mistral", p(Kind::Transcriptions, "https://api.mistral.ai/v1", "MISTRAL_API_KEY", "voxtral-mini-latest"),
         "Mistral Voxtral"),
        ("local-server", p(Kind::Transcriptions, "http://127.0.0.1:8000/v1", "", "whisper-1"),
         "A local OpenAI-compatible server (whisper.cpp server, Speaches, LocalAI…), no key"),
        ("local-command", Provider {
            kind: Kind::Command,
            command: "whisper-cli -m ~/models/ggml-large-v3-turbo.bin -l {lang} -nt -np -f {wav}".into(),
            ..Provider::default()
        }, "Any local program: gets the WAV path, prints the text"),
    ]
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// Name of the active profile in `providers`.
    pub provider: String,
    pub providers: BTreeMap<String, Provider>,
    /// Settings before providers existed (OpenRouter only); moved into `providers.openrouter`.
    #[serde(skip_serializing)]
    pub(crate) api_key: String,
    #[serde(skip_serializing)]
    pub(crate) model: String,
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
        let mut providers = BTreeMap::new();
        let (name, openrouter, _) = presets().remove(0);
        providers.insert(name.to_string(), openrouter);
        Self {
            provider: name.into(),
            providers,
            api_key: String::new(),
            model: String::new(),
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
        if self.api_key.is_empty() && self.model.is_empty() {
            return;
        }
        let entry = self.providers.entry("openrouter".into()).or_insert_with(|| presets().remove(0).1);
        if !self.api_key.is_empty() {
            entry.api_key = std::mem::take(&mut self.api_key);
        }
        if !self.model.is_empty() {
            entry.model = std::mem::take(&mut self.model);
        }
    }

    /// The active profile.
    pub fn active(&self) -> Result<&Provider> {
        self.providers.get(&self.provider).with_context(|| {
            crate::tf!("'{}' adlı sağlayıcı yok: `imla setup`", "no provider named '{}': run `imla setup`", self.provider)
        })
    }

    pub fn active_mut(&mut self) -> Result<&mut Provider> {
        let name = self.provider.clone();
        self.providers.get_mut(&name).with_context(|| crate::tf!("'{}' adlı sağlayıcı yok", "no provider named '{}'", name))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_config_moves_into_the_openrouter_profile() {
        let mut cfg: Config = toml::from_str("api_key = \"sk-test\"\nmodel = \"openai/gpt-audio\"\n").unwrap();
        cfg.migrate_legacy();
        let p = cfg.active().unwrap();
        assert_eq!(cfg.provider, "openrouter");
        assert_eq!((p.api_key.as_str(), p.model.as_str()), ("sk-test", "openai/gpt-audio"));
        assert!(!toml::to_string(&cfg).unwrap().contains("\napi_key = \"sk-test\"\nmodel"));
    }

    #[test]
    fn profiles_round_trip() {
        let mut cfg = Config::default();
        for (name, p, _) in presets() {
            cfg.providers.insert(name.into(), p);
        }
        cfg.provider = "local-command".into();
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(back.active().unwrap().kind, Kind::Command);
        assert_eq!(back.providers.len(), presets().len());
    }
}
