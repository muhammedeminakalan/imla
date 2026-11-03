//! imla: hold a shortcut, speak, release; the speech is transcribed by an OpenRouter audio
//! model and pasted into the focused window. KDE Plasma (Wayland) first.

mod config;
mod hud;
mod i18n;
mod local;
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
    config::{APP_ID, Config, Engine},
    hud::HudMsg,
    paste::Paster,
};

const SHORTCUT_ID: &str = "imla";
/// The portal learns the app id from `register_host_app` (tested: shortcut and keyboard grant
/// survive without the `app-<id>.service` naming).
const UNIT: &str = "imla.service";
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
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        shortcut: Option<String>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long)]
        terminal_paste: bool,
    },
    Status,
    Use { name: String },
    Model { model: Option<String> },
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
    /// HUD animasyonunu dosyaya yaz (README görseli için).
    #[command(hide = true)]
    RenderDemo { out: PathBuf, #[arg(long, default_value_t = 3)] scale: u32 },
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("imla=info,zbus=error")).init();
    config::migrate_old_name();
    match parse_cli().cmd {
        Cmd::Setup { provider, model, shortcut, language, terminal_paste } => {
            setup(shortcut, language, terminal_paste, provider, model).await
        }
        Cmd::Use { name } => use_provider(&name),
        Cmd::Model { model } => set_model(model).await,
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
            let bytes = fs::read(&wav)?;
            let engine = load_engine(&cfg).await?;
            println!("{}", speech_to_text(&cfg, engine.as_ref(), local::read_wav(&bytes)?, &bytes).await?);
            Ok(())
        }
        Cmd::RenderDemo { out, scale } => {
            let (w, h) = hud::render_demo(&out, scale, 30, [0x24, 0x29, 0x2f])?;
            println!("{w}x{h} rgb24 30fps");
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
            c.about(t("Sağlayıcıyı ve modeli seç, anahtarı kaydet, kısayolu ve servisi kur", "Choose the provider and model, save the key, set up the shortcut and the service"))
                .help_template(sub.clone())
                .override_usage(t("imla setup [SEÇENEKLER]", "imla setup [OPTIONS]"))
                .mut_arg("provider", |a| a.help(t("openrouter (bulut) ya da local (bu bilgisayarda, internetsiz)", "openrouter (cloud) or local (on this machine, offline)")).value_name(t("AD", "NAME")))
                .mut_arg("model", |a| a.help(t("OpenRouter model adı ya da yerel model (large-v3-turbo, small…)", "OpenRouter model id or a local model (large-v3-turbo, small…)")).value_name("MODEL"))
                .mut_arg("shortcut", |a| a.help(t("Tercih edilen kısayol (XDG biçimi), ör. \"CTRL+ALT+D\"", "Preferred shortcut (XDG syntax), e.g. \"CTRL+ALT+D\"")).value_name(t("TUŞLAR", "KEYS")))
                .mut_arg("language", |a| a.help(t("Konuşma dili: auto (kendisi anlar) ya da ISO kodu, ör. tr, en, de", "Speech language: auto (detected) or an ISO code, e.g. tr, en, de")).value_name(t("DİL", "LANG")))
                .mut_arg("terminal_paste", |a| a.help(t("Ctrl+Shift+V ile yapıştır (terminal ağırlıklı çalışıyorsan)", "Paste with Ctrl+Shift+V (if you mostly work in terminals)")))
        })
        .mut_subcommand("use", |c| {
            c.about(t("openrouter ile local arasında geç", "Switch between openrouter and local"))
                .help_template(sub.clone())
                .override_usage(t("imla use <AD>", "imla use <NAME>"))
                .mut_arg("name", |a| a.help("openrouter | local").value_name(t("AD", "NAME")))
        })
        .mut_subcommand("model", |c| {
            c.about(t("Etkin sağlayıcının modelini göster ya da değiştir", "Show or change the active provider's model"))
                .help_template(sub.clone())
                .override_usage("imla model [MODEL]")
                .mut_arg("model", |a| a.help(t("Yeni model (boşsa gösterir)", "New model (shows it when omitted)")).value_name("MODEL"))
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
    if cfg.provider == Engine::Openrouter && cfg.openrouter.key().is_empty() {
        bail!("{}", i18n::t("ayar yok: önce `imla setup` çalıştır", "not set up: run `imla setup` first"));
    }
    // the local model is loaded once and kept in memory
    let engine = load_engine(&cfg).await?;
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
                let (cfg, hud, paster, conn, busy, engine) =
                    (cfg.clone(), hud.clone(), paster.clone(), conn.clone(), busy.clone(), engine.clone());
                tokio::spawn(async move {
                    let samples = a.rec.stop().await;
                    let job = finish(&cfg, engine.as_ref(), &hud, &paster, conn, samples);
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

/// The local model when the local engine is selected (downloaded at setup).
async fn load_engine(cfg: &Config) -> Result<Option<Arc<local::Local>>> {
    if cfg.provider != Engine::Local {
        return Ok(None);
    }
    let path = local::model_path(&cfg.local.model)?;
    if !path.exists() {
        bail!("{}", tf!("yerel model yok: `imla setup` ile indir ({})", "the local model is missing: download it with `imla setup` ({})", path.display()));
    }
    let started = Instant::now();
    let engine = tokio::task::spawn_blocking(move || local::Local::load(&path)).await??;
    log::info!("{}", tf!("yerel model yüklendi ({:.1} sn)", "local model loaded ({:.1} s)", started.elapsed().as_secs_f32()));
    Ok(Some(Arc::new(engine)))
}

async fn speech_to_text(cfg: &Config, engine: Option<&Arc<local::Local>>, samples: Vec<i16>, wav: &[u8]) -> Result<String> {
    match engine {
        Some(e) => {
            let (e, lang, terms) = (e.clone(), cfg.language.clone(), cfg.terms.clone());
            tokio::task::spawn_blocking(move || e.transcribe(&samples, &lang, &terms)).await?
        }
        None => transcribe::openrouter(cfg, wav).await,
    }
}

async fn finish(
    cfg: &Config,
    engine: Option<&Arc<local::Local>>,
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
    let text = speech_to_text(cfg, engine, samples.clone(), &wav).await?;
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




/// Reads one line; an empty answer keeps `default`.
fn ask(label: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{default}]: ");
    }
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    Ok(if line.is_empty() { default.to_string() } else { line.to_string() })
}

fn engine_name(e: Engine) -> &'static str {
    match e {
        Engine::Openrouter => "openrouter",
        Engine::Local => "local",
    }
}

fn parse_engine(name: &str) -> Result<Engine> {
    match name.trim().to_lowercase().as_str() {
        "1" | "openrouter" => Ok(Engine::Openrouter),
        "2" | "local" | "yerel" => Ok(Engine::Local),
        other => bail!("{}", tf!("bilinmeyen sağlayıcı '{other}': openrouter ya da local", "unknown provider '{other}': openrouter or local")),
    }
}

/// Picks a local model by number or id; the current one is the default.
fn choose_local_model(current: &str) -> Result<String> {
    println!("{}", i18n::t("Yerel modeller:", "Local models:"));
    for (i, (id, _, mb, about)) in local::MODELS.iter().enumerate() {
        let mark = if *id == current { "*" } else { " " };
        let have = if local::model_path(id)?.exists() { i18n::t(" (indirilmiş)", " (downloaded)") } else { "" };
        println!(" {mark}{}. {id:<18} {mb:>4} MB  {about}{have}", i + 1);
    }
    let pick = ask(i18n::t("Model (numara ya da ad)", "Model (number or name)"), current)?;
    Ok(match pick.parse::<usize>() {
        Ok(i) if (1..=local::MODELS.len()).contains(&i) => local::MODELS[i - 1].0.to_string(),
        _ => pick,
    })
}

async fn setup(
    shortcut: Option<String>,
    language: Option<String>,
    terminal_paste: bool,
    provider: Option<String>,
    model: Option<String>,
) -> Result<()> {
    let mut cfg = Config::load()?;
    println!("{}\n", i18n::t("imla kurulumu", "imla setup"));
    let engine = match provider {
        Some(p) => parse_engine(&p)?,
        None => {
            println!("  1. OpenRouter  {}", i18n::t("bulutta ses modeli (anahtar gerekir)", "audio model in the cloud (needs a key)"));
            println!("  2. local       {}", i18n::t("bu bilgisayarda whisper.cpp, internetsiz", "whisper.cpp on this machine, offline"));
            parse_engine(&ask(i18n::t("Sağlayıcı", "Provider"), engine_name(cfg.provider))?)?
        }
    };
    match engine {
        Engine::Openrouter => {
            let label = if cfg.openrouter.key().is_empty() {
                i18n::t("OpenRouter API anahtarı: ", "OpenRouter API key: ")
            } else {
                i18n::t("OpenRouter API anahtarı (aynısı kalsın diye boş bırak): ", "OpenRouter API key (leave empty to keep it): ")
            };
            let key = rpassword::prompt_password(label)?;
            if !key.trim().is_empty() {
                cfg.openrouter.api_key = key.trim().to_string();
            }
            check_key(&cfg.openrouter.key()).await?;
            cfg.openrouter.model = match model {
                Some(m) => m,
                None => ask(i18n::t("Model", "Model"), &cfg.openrouter.model)?,
            };
        }
        Engine::Local => {
            let id = match model {
                Some(m) => m,
                None => choose_local_model(&cfg.local.model)?,
            };
            local::ensure(&id).await?;
            cfg.local.model = id;
        }
    }
    cfg.provider = engine;
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
        let line = ask(i18n::t(
            "Doğru yazılmasını istediğin adlar/terimler (virgülle, boş geçebilirsin)",
            "Names/terms to spell exactly (comma-separated, may be empty)",
        ), "")?;
        cfg.terms = line.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    cfg.save()?;
    println!("{}", tf!("Ayarlar kaydedildi: {}", "Settings saved: {}", config::path().display()));

    systemctl(&["daemon-reload"])?;
    // services under earlier names, if still enabled
    for old in ["app-dev.akalan.dictate.service", "app-dev.akalan.imla.service"] {
        let _ = systemctl(&["disable", "--now", old]);
    }
    systemctl(&["enable", UNIT])?;
    systemctl(&["restart", UNIT])?;
    let sc = &cfg.shortcut;
    println!(
        "{}",
        tf!(
            "\nServis başladı. KDE şimdi iki pencere açabilir:\n  1. Kısayolu onayla (önerilen: {sc}; istersen değiştir).\n  \
             2. Klavye denetimine izin ver (yapıştırmak için; bir kez sorulur).\n\n\
             Kullanım: kısayola basılı tut, konuş, bırak. Kısa bas: konuşmayı bitirince yine kısa bas.\n\
             Durum: `imla status` · Kayıtlar: `journalctl --user -u imla -f`",
            "\nThe service started. KDE may now open two dialogs:\n  1. Confirm the shortcut (suggested: {sc}; change it if you like).\n  \
             2. Allow keyboard control (for pasting; asked once).\n\n\
             Use: hold the shortcut, speak, release. Tap: tap again when you are done speaking.\n\
             Status: `imla status` · Logs: `journalctl --user -u imla -f`"
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

/// Switches between OpenRouter and the local model.
fn use_provider(name: &str) -> Result<()> {
    let mut cfg = Config::load()?;
    let engine = parse_engine(name)?;
    match engine {
        Engine::Openrouter if cfg.openrouter.key().is_empty() => {
            bail!("{}", i18n::t("OpenRouter anahtarı yok: `imla setup --provider openrouter`", "no OpenRouter key: run `imla setup --provider openrouter`"))
        }
        Engine::Local if !local::model_path(&cfg.local.model)?.exists() => {
            bail!("{}", i18n::t("yerel model indirilmemiş: `imla setup --provider local`", "the local model is not downloaded: run `imla setup --provider local`"))
        }
        _ => {}
    }
    cfg.provider = engine;
    cfg.save()?;
    restart_if_running();
    println!("{}", tf!("Etkin sağlayıcı: {}", "Active provider: {}", engine_name(engine)));
    Ok(())
}

/// Shows or sets the active engine's model (a local one is downloaded first).
async fn set_model(model: Option<String>) -> Result<()> {
    let mut cfg = Config::load()?;
    let name = engine_name(cfg.provider);
    let Some(m) = model else {
        match cfg.provider {
            Engine::Openrouter => println!("{name}: {}", cfg.openrouter.model),
            Engine::Local => {
                println!("{name}: {}", cfg.local.model);
                let ids: Vec<_> = local::MODELS.iter().map(|m| m.0).collect();
                println!("{}", tf!("Seçenekler: {}", "Choices: {}", ids.join(", ")));
            }
        }
        return Ok(());
    };
    match cfg.provider {
        Engine::Openrouter => cfg.openrouter.model = m.clone(),
        Engine::Local => {
            local::ensure(&m).await?;
            cfg.local.model = m.clone();
        }
    }
    cfg.save()?;
    restart_if_running();
    println!("{}", tf!("{name} modeli: {m}", "{name} model: {m}"));
    Ok(())
}

fn restart_if_running() {
    let _ = Command::new("systemctl").args(["--user", "try-restart", UNIT]).status();
}

fn systemctl(args: &[&str]) -> Result<()> {
    let ok = Command::new("systemctl").arg("--user").args(args).status()?.success();
    if !ok {
        bail!("{}", tf!("systemctl --user {} başarısız", "systemctl --user {} failed", args.join(" ")));
    }
    Ok(())
}
