//! imla: hold a shortcut, speak, release; the speech is transcribed by an OpenRouter audio
//! model and pasted into the focused window. KDE Plasma (Wayland) first.

mod config;
mod hud;
mod i18n;
mod paste;
mod recorder;
mod sounds;
mod transcribe;

use std::{
    fs,
    io::{BufRead, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use ashpd::{
    AppID,
    desktop::global_shortcuts::{GlobalShortcuts, NewShortcut},
};
use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::mpsc,
};

use crate::{
    config::{APP_ID, Config},
    hud::HudMsg,
    paste::Paster,
};

const SHORTCUT_ID: &str = "imla";
/// The portal also derives the app id from the unit name (app-<id>.service).
const UNIT: &str = "app-dev.akalan.imla.service";
/// A press shorter than this latches recording on until the next press (tap to start, tap to stop).
const TAP: Duration = Duration::from_millis(350);

#[derive(Parser)]
#[command(version, disable_help_subcommand = true, disable_help_flag = true, disable_version_flag = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    #[arg(short, long, action = clap::ArgAction::Help, global = true)]
    help: Option<bool>,
    #[arg(short = 'V', long, action = clap::ArgAction::Version)]
    version: Option<bool>,
}

#[derive(Subcommand)]
enum Cmd {
    Setup {
        #[arg(long)]
        shortcut: Option<String>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long)]
        terminal_paste: bool,
    },
    Status,
    /// Arka plan servisi (systemd çalıştırır).
    #[command(hide = true)]
    Daemon,
    /// Kaydı başlat ya da bitir (kısayolsuz).
    #[command(hide = true)]
    Toggle,
    /// Bir WAV dosyasını yazıya dök (deneme).
    #[command(hide = true)]
    Transcribe { wav: PathBuf },
    /// Göstergeyi birkaç saniye sahte sesle göster (görünüm denemesi).
    #[command(hide = true)]
    HudDemo,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("imla=info,zbus=error")).init();
    config::migrate_old_name();
    match parse_cli().cmd {
        Cmd::Setup { shortcut, language, terminal_paste } => setup(shortcut, language, terminal_paste).await,
        Cmd::Daemon => daemon().await,
        Cmd::Toggle => {
            println!("{}", reply_text(&control("toggle").await?));
            Ok(())
        }
        Cmd::Status => {
            let reply = control("status").await.unwrap_or_else(|_| "stopped".into());
            println!("{}", reply_text(&reply));
            Ok(())
        }
        Cmd::Transcribe { wav } => {
            let cfg = Config::load()?;
            println!("{}", transcribe::transcribe(&cfg, &fs::read(&wav)?).await?);
            Ok(())
        }
        Cmd::HudDemo => {
            let hud = hud::spawn();
            // listen -> transcribe -> done
            let _ = hud.send(HudMsg::Listen);
            for i in 0..180 {
                let _ = hud.send(HudMsg::Level(hud::demo_rms(i as f32 * 0.033)));
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
            let _ = hud.send(HudMsg::Busy);
            tokio::time::sleep(Duration::from_secs(3)).await;
            let _ = hud.send(HudMsg::Hide);
            tokio::time::sleep(Duration::from_millis(2000)).await;
            // listen -> error
            let _ = hud.send(HudMsg::Listen);
            for i in 0..45 {
                let _ = hud.send(HudMsg::Level(hud::demo_rms(i as f32 * 0.033)));
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
            let _ = hud.send(HudMsg::Error);
            tokio::time::sleep(Duration::from_millis(2000)).await;
            Ok(())
        }
    }
}

/// The CLI with its help in the session's language (clap's derive texts are fixed at build time).
fn parse_cli() -> Cli {
    use clap::{CommandFactory, FromArgMatches};
    let t = i18n::t;
    let (usage, cmds, opts) = (t("Kullanım", "Usage"), t("Komutlar", "Commands"), t("Seçenekler", "Options"));
    let top = format!("{{about}}\n\n{usage}: {{usage}}\n\n{cmds}:\n{{subcommands}}\n\n{opts}:\n{{options}}");
    let sub = format!("{{about}}\n\n{usage}: {{usage}}\n\n{opts}:\n{{options}}");
    let cmd = Cli::command()
        .about(t(
            "Kısayola basılı tut, konuş, bırak: söylediğin yazıya dökülüp yapıştırılır.",
            "Hold the shortcut, speak, release: what you said is transcribed and pasted.",
        ))
        .help_template(top)
        .override_usage(t("imla <KOMUT>", "imla <COMMAND>"))
        .mut_arg("help", |a| a.help(t("Yardımı göster", "Show help")))
        .mut_arg("version", |a| a.help(t("Sürümü göster", "Show version")))
        .mut_subcommand("setup", |c| {
            c.about(t("API anahtarını kaydet, kısayolu ve arka plan servisini kur", "Save the API key, set up the shortcut and the background service"))
                .help_template(sub.clone())
                .override_usage(t("imla setup [SEÇENEKLER]", "imla setup [OPTIONS]"))
                .mut_arg("shortcut", |a| a.help(t("Tercih edilen kısayol (XDG biçimi), ör. \"CTRL+ALT+D\"", "Preferred shortcut (XDG syntax), e.g. \"CTRL+ALT+D\"")).value_name(t("TUŞLAR", "KEYS")))
                .mut_arg("language", |a| a.help(t("Konuşma dili: auto (kendisi anlar) ya da ISO kodu, ör. tr, en, de", "Speech language: auto (detected) or an ISO code, e.g. tr, en, de")).value_name(t("DİL", "LANG")))
                .mut_arg("terminal_paste", |a| a.help(t("Ctrl+Shift+V ile yapıştır (terminal ağırlıklı çalışıyorsan)", "Paste with Ctrl+Shift+V (if you mostly work in terminals)")))
        })
        .mut_subcommand("status", |c| {
            c.about(t("Servis çalışıyor mu, ne yapıyor", "Whether the service runs and what it is doing"))
                .help_template(sub.clone())
                .override_usage("imla status")
        });
    let matches = cmd.get_matches();
    Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())
}

fn reply_text(key: &str) -> &'static str {
    match key {
        "ok" => i18n::t("tamam", "ok"),
        "listening" => i18n::t("dinliyor", "listening"),
        "transcribing" => i18n::t("yazıya döküyor", "transcribing"),
        "ready" => i18n::t("hazır", "ready"),
        "stopped" => i18n::t("çalışmıyor", "not running"),
        _ => i18n::t("bilinmeyen yanıt", "unknown reply"),
    }
}

fn socket_path() -> PathBuf {
    dirs::runtime_dir().unwrap_or_else(std::env::temp_dir).join("imla.sock")
}

async fn control(cmd: &str) -> Result<String> {
    let mut s = UnixStream::connect(socket_path()).await.context(i18n::t("servis çalışmıyor", "the service is not running"))?;
    s.write_all(format!("{cmd}\n").as_bytes()).await?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).await?;
    Ok(line.trim().to_string())
}

// ---------------------------------------------------------------- daemon

enum Event {
    Press,
    Release,
    Toggle,
}

struct Active {
    rec: recorder::Recording,
    pressed: Instant,
    latched: bool,
}

type Hud = smithay_client_toolkit::reexports::calloop::channel::Sender<HudMsg>;
/// The RemoteDesktop session, opened lazily and reopened when it breaks; pastes are serialised.
type SharedPaster = Arc<tokio::sync::Mutex<Option<Paster>>>;

async fn daemon() -> Result<()> {
    let cfg = Arc::new(Config::load()?);
    if cfg.api_key.is_empty() {
        bail!("{}", i18n::t("ayar yok: önce `imla setup` çalıştır", "not set up: run `imla setup` first"));
    }
    let conn = ashpd::zbus::Connection::session().await?;
    // without the app id KDE can't remember the shortcut; fail so systemd retries
    ashpd::register_host_app_with_connection(conn.clone(), AppID::try_from(APP_ID)?)
        .await
        .context(i18n::t("portal kaydı yapılamadı", "could not register with the portal"))?;
    let hud = hud::spawn();

    // shortcut and control socket first: they must work while KDE's keyboard dialog is open
    let shortcuts = GlobalShortcuts::with_connection(conn.clone()).await?;
    let session = shortcuts.create_session(Default::default()).await?;
    let bound = shortcuts
        .bind_shortcuts(
            &session,
            &[NewShortcut::new(SHORTCUT_ID, i18n::t("Sesle yazdır (basılı tut, konuş, bırak)", "Dictate (hold, speak, release)"))
                .preferred_trigger(cfg.shortcut.as_str())],
            None,
            Default::default(),
        )
        .await?
        .response()
        .context(i18n::t("kısayol kaydedilemedi", "could not bind the shortcut"))?;
    for s in bound.shortcuts() {
        log::info!("{}", tf!("kısayol: {} = {}", "shortcut: {} = {}", s.id(), s.trigger_description()));
    }
    let mut pressed = shortcuts.receive_activated().await?;
    let mut released = shortcuts.receive_deactivated().await?;

    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let busy = Arc::new(AtomicUsize::new(0));
    let listening = Arc::new(AtomicBool::new(false));
    spawn_control_socket(tx.clone(), busy.clone(), listening.clone())?;

    let paster: SharedPaster = Arc::new(tokio::sync::Mutex::new(None));
    {
        let (paster, conn) = (paster.clone(), conn.clone());
        tokio::spawn(async move {
            let mut slot = paster.lock().await;
            if slot.is_none() {
                match Paster::connect(conn).await {
                    Ok(p) => *slot = Some(p),
                    Err(e) => log::warn!("{}", tf!("klavye izni yok, ilk yapıştırmada yeniden sorulacak: {e:#}", "no keyboard access, asking again at the first paste: {e:#}")),
                }
            }
        });
    }

    let mut active: Option<Active> = None;
    let mut deadline = tokio::time::Instant::now();
    log::info!("{}", i18n::t("hazır", "ready"));
    loop {
        let event = tokio::select! {
            Some(a) = pressed.next() => if a.shortcut_id() == SHORTCUT_ID { Event::Press } else { continue },
            Some(d) = released.next() => if d.shortcut_id() == SHORTCUT_ID { Event::Release } else { continue },
            Some(e) = rx.recv() => e,
            _ = tokio::time::sleep_until(deadline), if active.is_some() => Event::Toggle,
        };
        match (event, active.take()) {
            (Event::Press | Event::Toggle, None) => {
                // recording is independent of an earlier clip still being transcribed
                let (ltx, mut lrx) = mpsc::unbounded_channel();
                match recorder::start(ltx) {
                    Ok(rec) => {
                        let h = hud.clone();
                        tokio::spawn(async move {
                            while let Some(l) = lrx.recv().await {
                                let _ = h.send(HudMsg::Level(l));
                            }
                        });
                        let _ = hud.send(HudMsg::Listen);
                        sounds::play(&cfg, sounds::Sound::Start);
                        listening.store(true, Ordering::SeqCst);
                        deadline = tokio::time::Instant::now() + Duration::from_secs(cfg.max_seconds);
                        active = Some(Active { rec, pressed: Instant::now(), latched: false });
                    }
                    Err(e) => {
                        log::error!("{e:#}");
                        let _ = hud.send(HudMsg::Error);
                    }
                }
            }
            (Event::Release, Some(a)) if !a.latched && a.pressed.elapsed() < TAP => {
                active = Some(Active { latched: true, ..a });
            }
            (Event::Release, Some(a)) if a.latched => active = Some(a),
            // a repeated press while the key is still held must not end the recording
            (Event::Press, Some(a)) if !a.latched => active = Some(a),
            (_, Some(a)) => {
                listening.store(false, Ordering::SeqCst);
                sounds::play(&cfg, sounds::Sound::Stop);
                busy.fetch_add(1, Ordering::SeqCst);
                let (cfg, hud, paster, conn, busy) =
                    (cfg.clone(), hud.clone(), paster.clone(), conn.clone(), busy.clone());
                tokio::spawn(async move {
                    let samples = a.rec.stop().await;
                    let job = finish(&cfg, &hud, &paster, conn, samples);
                    match tokio::time::timeout(Duration::from_secs(150), job).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            log::error!("{e:#}");
                            let _ = hud.send(HudMsg::Error);
                        }
                        Err(_) => {
                            log::error!("{}", i18n::t("zaman aşımı: yazıya dökme ya da yapıştırma bitmedi", "timed out: transcription or paste did not finish"));
                            let _ = hud.send(HudMsg::Error);
                        }
                    }
                    busy.fetch_sub(1, Ordering::SeqCst);
                });
            }
            (Event::Release, None) => {}
        }
    }
}

async fn finish(
    cfg: &Config,
    hud: &Hud,
    paster: &SharedPaster,
    conn: ashpd::zbus::Connection,
    samples: Vec<i16>,
) -> Result<()> {
    if recorder::is_silent(&samples) {
        let _ = hud.send(HudMsg::Hide);
        sounds::play(cfg, sounds::Sound::Cancel);
        return Ok(());
    }
    let _ = hud.send(HudMsg::Busy);
    let started = Instant::now();
    let wav = recorder::wav(&samples);
    save_last_clip(&wav);
    let text = transcribe::transcribe(cfg, &wav).await?;
    let (secs, took, chars) =
        (samples.len() as f32 / recorder::SAMPLE_RATE as f32, started.elapsed().as_secs_f32(), text.chars().count());
    log::info!(
        "{}",
        tf!("{secs:.1} sn ses, {took:.1} sn'de çözüldü: {chars} karakter", "{secs:.1} s of audio, transcribed in {took:.1} s: {chars} characters")
    );
    let _ = hud.send(HudMsg::Hide);
    if text.is_empty() {
        return Ok(());
    }
    let mut slot = paster.lock().await;
    for attempt in 0..2 {
        if slot.is_none() {
            match Paster::connect(conn.clone()).await {
                Ok(p) => *slot = Some(p),
                Err(e) => {
                    paste::set_clipboard(text).await?;
                    bail!("{}", tf!("klavye izni yok, metin panoda bırakıldı: {e:#}", "no keyboard access, the text is on the clipboard: {e:#}"));
                }
            }
        }
        match slot.as_ref().unwrap().paste(cfg, &text).await {
            Ok(()) => return Ok(()),
            Err(e) if attempt == 0 => {
                log::warn!("{}", tf!("yapıştırma oturumu kopmuş, yeniden açılıyor: {e:#}", "paste session lost, reopening: {e:#}"));
                *slot = None;
            }
            Err(e) => return Err(e.context(i18n::t("yapıştırılamadı, metin panoda", "could not paste, the text is on the clipboard"))),
        }
    }
    Ok(())
}

/// Keeps the latest clip (owner-only, overwritten each time) so a bad result can be checked.
fn save_last_clip(wav: &[u8]) {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = config::state_dir();
    let _ = fs::create_dir_all(&dir);
    let written = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(dir.join("last.wav"))
        .and_then(|mut f| f.write_all(wav));
    if let Err(e) = written {
        log::warn!("{}", tf!("son kayıt saklanamadı: {e}", "could not keep the last clip: {e}"));
    }
}

fn spawn_control_socket(
    tx: mpsc::UnboundedSender<Event>,
    busy: Arc<AtomicUsize>,
    listening: Arc<AtomicBool>,
) -> Result<()> {
    let path = socket_path();
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (tx, busy, listening) = (tx.clone(), busy.clone(), listening.clone());
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut line = String::new();
                if BufReader::new(r).read_line(&mut line).await.is_err() {
                    return;
                }
                let reply = match line.trim() {
                    // replies are keys; the CLI words them in its own language
                    "toggle" => {
                        let _ = tx.send(Event::Toggle);
                        "ok"
                    }
                    "status" if listening.load(Ordering::SeqCst) => "listening",
                    "status" if busy.load(Ordering::SeqCst) > 0 => "transcribing",
                    "status" => "ready",
                    _ => "unknown",
                };
                let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
            });
        }
    });
    Ok(())
}

// ---------------------------------------------------------------- setup




async fn setup(shortcut: Option<String>, language: Option<String>, terminal_paste: bool) -> Result<()> {
    let mut cfg = Config::load()?;
    println!("{}\n", i18n::t("imla kurulumu", "imla setup"));
    let key = rpassword::prompt_password(if cfg.api_key.is_empty() {
        i18n::t("OpenRouter API anahtarı: ", "OpenRouter API key: ")
    } else {
        i18n::t("OpenRouter API anahtarı (aynısı kalsın diye boş bırak): ", "OpenRouter API key (leave empty to keep it): ")
    })?;
    if !key.trim().is_empty() {
        cfg.api_key = key.trim().to_string();
    }
    check_key(&cfg.api_key).await?;
    if let Some(s) = shortcut {
        cfg.shortcut = s;
    }
    if let Some(l) = language {
        cfg.language = l.trim().to_lowercase();
    }
    if terminal_paste {
        cfg.paste_keys = "ctrl+shift+v".into();
    }
    if cfg.terms.is_empty() {
        print!("{}", i18n::t(
            "Doğru yazılmasını istediğin adlar/terimler (virgülle, boş geçebilirsin): ",
            "Names/terms to spell exactly (comma-separated, may be empty): "
        ));
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        cfg.terms = line.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    cfg.save()?;
    println!("{}", tf!("Ayarlar kaydedildi: {}", "Settings saved: {}", config::path().display()));

    systemctl(&["daemon-reload"])?;
    // the service under the project's former name, if it is still enabled
    let _ = systemctl(&["disable", "--now", "app-dev.akalan.dictate.service"]);
    systemctl(&["enable", UNIT])?;
    systemctl(&["restart", UNIT])?;
    let sc = &cfg.shortcut;
    println!(
        "{}",
        tf!(
            "\nServis başladı. KDE şimdi iki pencere açabilir:\n  1. Kısayolu onayla (önerilen: {sc}; istersen değiştir).\n  \
             2. Klavye denetimine izin ver (yapıştırmak için; bir kez sorulur).\n\n\
             Kullanım: kısayola basılı tut, konuş, bırak. Kısa bas: konuşmayı bitirince yine kısa bas.\n\
             Durum: `imla status` · Kayıtlar: `journalctl --user -u app-dev.akalan.imla -f`",
            "\nThe service started. KDE may now open two dialogs:\n  1. Confirm the shortcut (suggested: {sc}; change it if you like).\n  \
             2. Allow keyboard control (for pasting; asked once).\n\n\
             Use: hold the shortcut, speak, release. Tap: tap again when you are done speaking.\n\
             Status: `imla status` · Logs: `journalctl --user -u app-dev.akalan.imla -f`"
        )
    );
    Ok(())
}

async fn check_key(key: &str) -> Result<()> {
    let r = reqwest::Client::new()
        .get("https://openrouter.ai/api/v1/key")
        .bearer_auth(key)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .context(i18n::t("OpenRouter'a bağlanılamadı", "could not reach OpenRouter"))?;
    if !r.status().is_success() {
        bail!("{}", tf!("anahtar geçersiz (OpenRouter {})", "invalid key (OpenRouter {})", r.status()));
    }
    println!("{}", i18n::t("Anahtar doğrulandı.", "Key verified."));
    Ok(())
}

fn systemctl(args: &[&str]) -> Result<()> {
    let ok = Command::new("systemctl").arg("--user").args(args).status()?.success();
    if !ok {
        bail!("{}", tf!("systemctl --user {} başarısız", "systemctl --user {} failed", args.join(" ")));
    }
    Ok(())
}
