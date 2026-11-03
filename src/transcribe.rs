//! Speech to text through an audio-capable chat model on OpenRouter.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};

use crate::config::Config;

fn prompt(cfg: &Config) -> String {
    let mut p = if cfg.language == "tr" {
        "Bu ses kaydını Türkçe olarak kelimesi kelimesine yazıya dök. Noktalama ve büyük harf kullan. \
         Yalnızca dökümü yaz; açıklama, tırnak ya da başlık ekleme. Kayıt boşsa ya da konuşma yoksa hiçbir şey yazma."
            .to_string()
    } else {
        format!(
            "Transcribe this recording verbatim in the language with ISO code '{}'. Use punctuation and \
             capitalization. Output only the transcript, nothing else; output nothing if there is no speech.",
            cfg.language
        )
    };
    if !cfg.terms.is_empty() {
        p.push_str(if cfg.language == "tr" {
            "\nŞu adlar ve terimler geçebilir, geçerse tam böyle yaz: "
        } else {
            "\nThese names and terms may occur; spell them exactly like this: "
        });
        p.push_str(&cfg.terms.join(", "));
    }
    p
}

pub async fn transcribe(cfg: &Config, wav: &[u8]) -> Result<String> {
    if cfg.api_key.is_empty() {
        bail!("OpenRouter anahtarı yok: `dictate setup` çalıştır");
    }
    let body = json!({
        "model": cfg.model,
        "temperature": 0,
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": prompt(cfg)},
            {"type": "input_audio", "input_audio": {
                "data": base64::engine::general_purpose::STANDARD.encode(wav), "format": "wav"}},
        ]}],
    });
    let resp = reqwest::Client::new()
        .post("https://openrouter.ai/api/v1/chat/completions")
        .bearer_auth(&cfg.api_key)
        .header("X-Title", "dictate")
        .timeout(Duration::from_secs(90))
        .json(&body)
        .send()
        .await
        .context("OpenRouter'a bağlanılamadı")?;
    let status = resp.status();
    let v: Value = resp.json().await.context("OpenRouter yanıtı okunamadı")?;
    if !status.is_success() {
        bail!("OpenRouter {status}: {}", v["error"]["message"].as_str().unwrap_or("bilinmeyen hata"));
    }
    let text = v["choices"][0]["message"]["content"].as_str().unwrap_or_default();
    Ok(clean(text))
}

/// Drops wrapping quotes or code fences some models add despite the prompt.
fn clean(text: &str) -> String {
    let t = text.trim().trim_start_matches("```").trim_end_matches("```").trim();
    let t = t.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(t);
    t.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_strips_quotes_and_fences() {
        assert_eq!(clean("  \"Merhaba dünya.\" "), "Merhaba dünya.");
        assert_eq!(clean("```\nSelam\n```"), "Selam");
        assert_eq!(clean("Düz metin"), "Düz metin");
    }

    #[test]
    fn prompt_lists_terms() {
        let cfg = Config { terms: vec!["Gülendam Afra".into(), "JSON".into()], ..Config::default() };
        assert!(prompt(&cfg).contains("Gülendam Afra, JSON"));
    }
}
