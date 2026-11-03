//! Offline transcription with whisper.cpp (whisper-rs). Models are ggml files from
//! huggingface.co/ggerganov/whisper.cpp, downloaded once into ~/.cache/imla/models.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::{i18n::t, tf};

pub const DEFAULT_MODEL: &str = "large-v3-turbo";

/// id, file, size in MB, description.
pub const MODELS: &[(&str, &str, u32, &str)] = &[
    ("large-v3-turbo", "ggml-large-v3-turbo-q5_0.bin", 547, "best quality/speed balance (default)"),
    ("large-v3-turbo-q8", "ggml-large-v3-turbo-q8_0.bin", 833, "slightly more accurate, larger"),
    ("medium", "ggml-medium-q5_0.bin", 514, "older, slower than turbo"),
    ("small", "ggml-small-q5_1.bin", 181, "fast, less accurate"),
    ("base", "ggml-base-q5_1.bin", 56, "fastest, rough"),
];

fn entry(id: &str) -> Result<&'static (&'static str, &'static str, u32, &'static str)> {
    MODELS.iter().find(|m| m.0 == id).with_context(|| {
        let ids: Vec<_> = MODELS.iter().map(|m| m.0).collect();
        tf!("bilinmeyen yerel model '{id}' (seçenekler: {})", "unknown local model '{id}' (choices: {})", ids.join(", "))
    })
}

pub fn models_dir() -> PathBuf {
    dirs::cache_dir().expect("no cache dir").join("imla/models")
}

pub fn model_path(id: &str) -> Result<PathBuf> {
    Ok(models_dir().join(entry(id)?.1))
}

/// Downloads the model if it is not there yet, printing progress; returns its path.
pub async fn ensure(id: &str) -> Result<PathBuf> {
    let (_, file, size, _) = *entry(id)?;
    let path = models_dir().join(file);
    if path.exists() {
        return Ok(path);
    }
    fs::create_dir_all(models_dir())?;
    let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}");
    println!("{}", tf!("{id} modeli indiriliyor ({size} MB)…", "Downloading the {id} model ({size} MB)…"));
    let mut resp = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(3600))
        .send()
        .await
        .context(t("Hugging Face'e bağlanılamadı", "could not reach Hugging Face"))?
        .error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let part = path.with_extension("part");
    let mut out = fs::File::create(&part)?;
    let (mut got, mut shown) = (0u64, 0u64);
    while let Some(chunk) = resp.chunk().await? {
        out.write_all(&chunk)?;
        got += chunk.len() as u64;
        if total > 0 && got * 20 / total > shown {
            shown = got * 20 / total;
            print!("\r  {:>3}%", got * 100 / total);
            std::io::stdout().flush()?;
        }
    }
    println!();
    out.sync_all()?;
    if total > 0 && got != total {
        let _ = fs::remove_file(&part);
        bail!("{}", t("indirme yarım kaldı, yeniden dene", "the download is incomplete, try again"));
    }
    fs::rename(&part, &path)?;
    Ok(path)
}

/// A loaded model; transcription is blocking (run it off the async runtime).
pub struct Local {
    ctx: WhisperContext,
}

impl Local {
    pub fn load(path: &Path) -> Result<Self> {
        whisper_rs::install_logging_hooks(); // keep whisper.cpp's chatter out of the journal
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .with_context(|| tf!("model yüklenemedi: {}", "could not load the model: {}", path.display()))?;
        Ok(Self { ctx })
    }

    /// 16 kHz mono PCM in, text out. `language`: ISO code or "auto"; `terms` steer spelling.
    pub fn transcribe(&self, samples: &[i16], language: &str, terms: &[String]) -> Result<String> {
        let mut audio = vec![0f32; samples.len()];
        whisper_rs::convert_integer_to_float_audio(samples, &mut audio)?;
        let mut state = self.ctx.create_state()?;
        let mut params = FullParams::new(SamplingStrategy::BeamSearch { beam_size: 5, patience: -1.0 });
        let lang = if language.is_empty() { "auto" } else { language };
        params.set_language(Some(lang));
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
        params.set_n_threads(threads as i32);
        params.set_no_context(true);
        params.set_no_timestamps(true);
        params.set_suppress_blank(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        let prompt = terms.join(", ");
        if !prompt.is_empty() {
            params.set_initial_prompt(&prompt);
        }
        state.full(params, &audio)?;
        let mut text = String::new();
        for segment in state.as_iter() {
            text.push_str(&segment.to_str_lossy()?);
        }
        Ok(text.trim().to_string())
    }
}

/// PCM samples of a 16-bit mono WAV file (for `imla transcribe`).
pub fn read_wav(bytes: &[u8]) -> Result<Vec<i16>> {
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32::from_le_bytes(bytes[i + 4..i + 8].try_into()?) as usize;
        if id == b"data" {
            let data = &bytes[i + 8..(i + 8 + len).min(bytes.len())];
            return Ok(data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect());
        }
        i += 8 + len + (len & 1);
    }
    bail!("{}", t("WAV dosyasında ses verisi yok", "no audio data in the WAV file"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_round_trip() {
        let samples = [0i16, 100, -100, i16::MAX];
        assert_eq!(read_wav(&crate::recorder::wav(&samples)).unwrap(), samples);
    }

    #[test]
    fn models_resolve_to_files() {
        assert!(model_path(DEFAULT_MODEL).unwrap().ends_with("ggml-large-v3-turbo-q5_0.bin"));
        assert!(model_path("nope").is_err());
    }
}
