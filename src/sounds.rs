//! Recording tones (start, stop, dropped), played through PipeWire's `pw-play`.

use std::{fs, path::PathBuf, process::Stdio};

use crate::config::Config;

#[derive(Clone, Copy)]
pub enum Sound {
    /// The microphone is open.
    Start,
    /// The recording ended and goes to transcription.
    Stop,
    /// The recording was dropped (silent or too short).
    Cancel,
}

impl Sound {
    fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Cancel => "cancel",
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Start => include_bytes!("../assets/sounds/start.wav"),
            Self::Stop => include_bytes!("../assets/sounds/stop.wav"),
            Self::Cancel => include_bytes!("../assets/sounds/cancel.wav"),
        }
    }

    /// The tone as a file pw-play can open, written once to the cache.
    fn file(self) -> Option<PathBuf> {
        let dir = dirs::cache_dir()?.join("imla/sounds");
        let path = dir.join(format!("{}.wav", self.name()));
        let current = fs::metadata(&path).map(|m| m.len() == self.bytes().len() as u64).unwrap_or(false);
        if !current {
            fs::create_dir_all(&dir).ok()?;
            fs::write(&path, self.bytes()).ok()?;
        }
        Some(path)
    }
}

/// Plays a tone without waiting for it; does nothing when sounds are off.
pub fn play(cfg: &Config, sound: Sound) {
    if !cfg.sound_effects || cfg.sound_volume <= 0.0 {
        return;
    }
    let Some(path) = sound.file() else { return };
    let spawned = std::process::Command::new("pw-play")
        .arg("--volume")
        .arg(format!("{:.2}", cfg.sound_volume.clamp(0.0, 1.0)))
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        // reap it in the background so no zombie is left behind
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => log::warn!("{}", crate::tf!("ses çalınamadı: {e}", "could not play a tone: {e}")),
    }
}
