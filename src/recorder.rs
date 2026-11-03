//! Microphone capture through PipeWire's `pw-record` (16 kHz mono s16), with a live level meter.

use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
};

pub const SAMPLE_RATE: u32 = 16_000;

pub struct Recording {
    child: Child,
    reader: JoinHandle<Vec<i16>>,
}

/// Starts recording; `levels` receives the loudness (0..1) of every ~32 ms chunk.
pub fn start(levels: mpsc::UnboundedSender<f32>) -> Result<Recording> {
    let mut child = Command::new("pw-record")
        .args(["--rate", &SAMPLE_RATE.to_string(), "--channels", "1", "--format", "s16", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("pw-record başlatılamadı (PipeWire araçları gerekli)")?;
    let mut out = child.stdout.take().context("pw-record çıktısı yok")?;
    let reader = tokio::spawn(async move {
        let mut samples = Vec::with_capacity(SAMPLE_RATE as usize * 30);
        let mut buf = vec![0u8; 1024];
        let mut carry: Option<u8> = None;
        loop {
            let n = match out.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let mut bytes: Vec<u8> = carry.take().into_iter().collect();
            bytes.extend_from_slice(&buf[..n]);
            if bytes.len() % 2 == 1 {
                carry = bytes.pop();
            }
            let chunk: Vec<i16> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
            let rms = (chunk.iter().map(|&s| (s as f32 / 32768.0).powi(2)).sum::<f32>()
                / chunk.len().max(1) as f32)
                .sqrt();
            // speech sits around 0.02-0.2 RMS; spread it over 0..1 for the meter
            let _ = levels.send((rms * 6.0).sqrt().min(1.0));
            samples.extend_from_slice(&chunk);
        }
        samples
    });
    Ok(Recording { child, reader })
}

impl Recording {
    /// Stops the microphone and returns the captured samples.
    pub async fn stop(mut self) -> Vec<i16> {
        // the last syllable is often still in flight: keep listening a moment, then end pw-record
        // with SIGINT so it flushes its buffers; SIGKILL only if it doesn't exit
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        if let Some(pid) = self.child.id() {
            unsafe { libc::kill(pid as i32, libc::SIGINT) };
        }
        if tokio::time::timeout(std::time::Duration::from_millis(500), self.child.wait()).await.is_err() {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }
        self.reader.await.unwrap_or_default()
    }
}

/// Wraps PCM samples in a WAV container.
pub fn wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    w.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

/// True when the clip is too short or too quiet to hold speech.
pub fn is_silent(samples: &[i16]) -> bool {
    if samples.len() < SAMPLE_RATE as usize / 3 {
        return true;
    }
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    peak < 800
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_matches_pcm() {
        let w = wav(&[0, 1, -1, i16::MAX]);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), SAMPLE_RATE);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 8);
        assert_eq!(w.len(), 44 + 8);
    }

    #[test]
    fn silence_and_short_clips_are_dropped() {
        assert!(is_silent(&[0; 100]));
        assert!(is_silent(&vec![50; SAMPLE_RATE as usize]));
        let mut speech = vec![0i16; SAMPLE_RATE as usize];
        speech[1000] = 5000;
        assert!(!is_silent(&speech));
    }
}
