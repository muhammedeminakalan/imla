//! User settings in ~/.config/dictate/config.toml (owner-only: it holds the API key).

use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "dev.akalan.dictate";

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// OpenRouter API key.
    pub api_key: String,
    /// OpenRouter model that takes audio input.
    pub model: String,
    /// Pure speech-to-text model (OpenRouter /audio/transcriptions) used when the chat model
    /// answers or refuses instead of transcribing; empty disables the retry.
    pub fallback_model: String,
    /// ISO-639-1 language of the speech.
    pub language: String,
    /// Words the model should spell exactly (names, jargon); sent as hints with every request.
    pub terms: Vec<String>,
    /// Preferred shortcut, in the XDG shortcuts syntax; KDE lets you change it when binding.
    pub shortcut: String,
    /// Keys that paste the transcript: "ctrl+v" or "ctrl+shift+v" (terminals).
    pub paste_keys: String,
    /// Put the previous clipboard text back after pasting.
    pub restore_clipboard: bool,
    /// Recording stops by itself after this many seconds.
    pub max_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "openai/gpt-audio-mini".into(),
            fallback_model: "openai/gpt-transcribe".into(),
            language: "tr".into(),
            terms: Vec::new(),
            shortcut: "CTRL+ALT+D".into(),
            paste_keys: "ctrl+v".into(),
            restore_clipboard: true,
            max_seconds: 120,
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().expect("no config dir").join("dictate")
}

pub fn state_dir() -> PathBuf {
    dirs::state_dir().expect("no state dir").join("dictate")
}

pub fn path() -> PathBuf {
    config_dir().join("config.toml")
}

impl Config {
    pub fn load() -> Result<Self> {
        let p = path();
        let mut cfg: Self = match fs::read_to_string(&p) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("{} okunamadı", p.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e).with_context(|| format!("{} okunamadı", p.display())),
        };
        if cfg.api_key.is_empty() {
            cfg.api_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
        }
        Ok(cfg)
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
