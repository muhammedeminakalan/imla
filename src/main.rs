//! dictate: hold a shortcut, speak, release; the speech is transcribed by an OpenRouter audio
//! model and pasted into the focused window. KDE Plasma (Wayland) first.

mod config;
mod hud;
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

const SHORTCUT_ID: &str = "dictate";
/// The portal also derives the app id from the unit name (app-<id>.service).
const UNIT: &str = "app-dev.akalan.dictate.service";
/// A press shorter than this latches recording on until the next press (tap to start, tap to stop).
const TAP: Duration = Duration::from_millis(350);

#[derive(Parser)]
#[command(version, about = "Kısayola basılı tut, konuş, bırak: söylediğin yazıya dökülüp yapıştırılır.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// API anahtarını kaydet, kısayolu ve arka plan servisini kur.
    Setup {
        /// Tercih edilen kısayol (XDG biçimi), ör. "CTRL+ALT+D".
        #[arg(long)]
        shortcut: Option<String>,
        /// Panoya yapıştırırken Ctrl+Shift+V kullan (terminal ağırlıklı çalışıyorsan).
        #[arg(long)]
        terminal_paste: bool,
    },
    /// Arka plan servisi (systemd çalıştırır).
    Daemon,
    /// Kaydı başlat ya da bitir (kısayolsuz kullanım, ör. başka bir tuşa bağlamak için).
    Toggle,
    /// Servis çalışıyor mu, ne yapıyor.
    Status,
    /// Bir WAV dosyasını yazıya dök (deneme için).
    Transcribe { wav: PathBuf },
    /// Servisi durdur, kaldır (ayarlar kalır).
    Uninstall,
    /// Göstergeyi birkaç saniye sahte sesle göster (görünüm denemesi).
    #[command(hide = true)]
    HudDemo,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("dictate=info,zbus=error")).init();
    match Cli::parse().cmd {
        Cmd::Setup { shortcut, terminal_paste } => setup(shortcut, terminal_paste).await,
        Cmd::Daemon => daemon().await,
        Cmd::Toggle => {
            println!("{}", control("toggle").await?);
            Ok(())
        }
        Cmd::Status => {
            println!("{}", control("status").await.unwrap_or_else(|_| "çalışmıyor".into()));
            Ok(())
        }
        Cmd::Transcribe { wav } => {
            let cfg = Config::load()?;
            println!("{}", transcribe::transcribe(&cfg, &fs::read(&wav)?).await?);
            Ok(())
        }
        Cmd::Uninstall => uninstall(),
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

fn socket_path() -> PathBuf {
    dirs::runtime_dir().unwrap_or_else(std::env::temp_dir).join("dictate.sock")
}

async fn control(cmd: &str) -> Result<String> {
    let mut s = UnixStream::connect(socket_path()).await.context("servis çalışmıyor")?;
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
        bail!("ayar yok: önce `dictate setup` çalıştır");
    }
    let conn = ashpd::zbus::Connection::session().await?;
    // without the app id KDE can't remember the shortcut; fail so systemd retries
    ashpd::register_host_app_with_connection(conn.clone(), AppID::try_from(APP_ID)?)
        .await
        .context("portal kaydı yapılamadı")?;
    let hud = hud::spawn();

    // shortcut and control socket first: they must work while KDE's keyboard dialog is open
    let shortcuts = GlobalShortcuts::with_connection(conn.clone()).await?;
    let session = shortcuts.create_session(Default::default()).await?;
    let bound = shortcuts
        .bind_shortcuts(
            &session,
            &[NewShortcut::new(SHORTCUT_ID, "Sesle yazdır (basılı tut, konuş, bırak)")
                .preferred_trigger(cfg.shortcut.as_str())],
            None,
            Default::default(),
        )
        .await?
        .response()
        .context("kısayol kaydedilemedi")?;
    for s in bound.shortcuts() {
        log::info!("kısayol: {} = {}", s.id(), s.trigger_description());
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
                    Err(e) => log::warn!("klavye izni yok, ilk yapıştırmada yeniden sorulacak: {e:#}"),
                }
            }
        });
    }

    let mut active: Option<Active> = None;
    let mut deadline = tokio::time::Instant::now();
    log::info!("hazır");
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
                            log::error!("zaman aşımı: yazıya dökme ya da yapıştırma bitmedi");
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
    log::info!(
        "{:.1} sn ses, {:.1} sn'de çözüldü: {} karakter",
        samples.len() as f32 / recorder::SAMPLE_RATE as f32,
        started.elapsed().as_secs_f32(),
        text.chars().count()
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
                    bail!("klavye izni yok, metin panoda bırakıldı: {e:#}");
                }
            }
        }
        match slot.as_ref().unwrap().paste(cfg, &text).await {
            Ok(()) => return Ok(()),
            Err(e) if attempt == 0 => {
                log::warn!("yapıştırma oturumu kopmuş, yeniden açılıyor: {e:#}");
                *slot = None;
            }
            Err(e) => return Err(e.context("yapıştırılamadı, metin panoda")),
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
        log::warn!("son kayıt saklanamadı: {e}");
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
                    "toggle" => {
                        let _ = tx.send(Event::Toggle);
                        "tamam"
                    }
                    "status" if listening.load(Ordering::SeqCst) => "dinliyor",
                    "status" if busy.load(Ordering::SeqCst) > 0 => "yazıya döküyor",
                    "status" => "hazır",
                    _ => "bilinmeyen komut",
                };
                let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
            });
        }
    });
    Ok(())
}

// ---------------------------------------------------------------- setup

fn bin_path() -> PathBuf {
    dirs::executable_dir().unwrap_or_else(|| dirs::home_dir().unwrap().join(".local/bin")).join("dictate")
}

fn unit_path() -> PathBuf {
    dirs::config_dir().unwrap().join(format!("systemd/user/{UNIT}"))
}

fn desktop_path() -> PathBuf {
    dirs::data_dir().unwrap().join(format!("applications/{APP_ID}.desktop"))
}

async fn setup(shortcut: Option<String>, terminal_paste: bool) -> Result<()> {
    let mut cfg = Config::load()?;
    println!("dictate kurulumu\n");
    let key = rpassword::prompt_password(if cfg.api_key.is_empty() {
        "OpenRouter API anahtarı: "
    } else {
        "OpenRouter API anahtarı (aynısı kalsın diye boş bırak): "
    })?;
    if !key.trim().is_empty() {
        cfg.api_key = key.trim().to_string();
    }
    check_key(&cfg.api_key).await?;
    if let Some(s) = shortcut {
        cfg.shortcut = s;
    }
    if terminal_paste {
        cfg.paste_keys = "ctrl+shift+v".into();
    }
    if cfg.terms.is_empty() {
        print!("Doğru yazılmasını istediğin adlar/terimler (virgülle, boş geçebilirsin): ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        cfg.terms = line.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    cfg.save()?;
    println!("Ayarlar kaydedildi: {}", config::path().display());

    if packaged() {
        remove_home_install(); // a copy from before the package would shadow it
    } else {
        install_files()?;
    }
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    systemctl(&["restart", UNIT])?;
    println!(
        "\nServis başladı. KDE şimdi iki pencere açabilir:\n  1. Kısayolu onayla (önerilen: {}; istersen değiştir).\n  \
         2. Klavye denetimine izin ver (yapıştırmak için; bir kez sorulur).\n\n\
         Kullanım: kısayola basılı tut, konuş, bırak. Kısa bas: konuşmayı bitirince yine kısa bas.\n\
         Durum: `dictate status` · Kayıtlar: `journalctl --user -u app-dev.akalan.dictate -f`",
        cfg.shortcut
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
        .context("OpenRouter'a bağlanılamadı")?;
    if !r.status().is_success() {
        bail!("anahtar geçersiz (OpenRouter {})", r.status());
    }
    println!("Anahtar doğrulandı.");
    Ok(())
}

/// Installed by the Arch package: binary in /usr/bin, unit and desktop entry under /usr.
fn packaged() -> bool {
    std::env::current_exe().map(|p| p.starts_with("/usr/")).unwrap_or(false)
}

/// Removes the per-user copy `setup` made before the package existed.
fn remove_home_install() {
    let _ = systemctl(&["disable", "--now", UNIT]);
    for p in [unit_path(), desktop_path(), bin_path()] {
        if p.exists() && fs::remove_file(&p).is_ok() {
            println!("Eski kurulum kaldırıldı: {}", p.display());
        }
    }
}

fn install_files() -> Result<()> {
    let bin = bin_path();
    let me = std::env::current_exe()?;
    if me.canonicalize().ok() != bin.canonicalize().ok() {
        fs::create_dir_all(bin.parent().unwrap())?;
        let tmp = bin.with_extension("new");
        fs::copy(&me, &tmp)?;
        fs::rename(&tmp, &bin)?;
        println!("Kuruldu: {}", bin.display());
    }
    let desktop = desktop_path();
    fs::create_dir_all(desktop.parent().unwrap())?;
    fs::write(
        &desktop,
        format!(
            "[Desktop Entry]\nType=Application\nName=dictate\nComment=Sesle yazdırma\n\
             Exec={} daemon\nIcon=audio-input-microphone\nNoDisplay=true\nTerminal=false\n",
            bin.display()
        ),
    )?;
    let unit = unit_path();
    fs::create_dir_all(unit.parent().unwrap())?;
    fs::write(
        &unit,
        format!(
            "[Unit]\nDescription=dictate: sesle yazdırma\nPartOf=graphical-session.target\n\
             After=graphical-session.target\nStartLimitIntervalSec=0\n\n[Service]\nExecStart={} daemon\nRestart=on-failure\n\
             RestartSec=3\n\n[Install]\nWantedBy=graphical-session.target\n",
            bin.display()
        ),
    )?;
    Ok(())
}

fn systemctl(args: &[&str]) -> Result<()> {
    let ok = Command::new("systemctl").arg("--user").args(args).status()?.success();
    if !ok {
        bail!("systemctl --user {} başarısız", args.join(" "));
    }
    Ok(())
}

fn uninstall() -> Result<()> {
    let _ = systemctl(&["disable", "--now", UNIT]);
    if packaged() {
        println!("Servis kapatıldı. Programı kaldırmak için: sudo pacman -R dictate");
        println!("Ayarlar duruyor: {}", config::path().display());
        return Ok(());
    }
    for p in [unit_path(), desktop_path(), bin_path()] {
        if p.exists() {
            fs::remove_file(&p)?;
            println!("Silindi: {}", p.display());
        }
    }
    let _ = systemctl(&["daemon-reload"]);
    println!("Ayarlar duruyor: {}", config::path().display());
    Ok(())
}
