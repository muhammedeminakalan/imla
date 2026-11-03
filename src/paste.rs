//! Pasting into the focused window on Wayland: the text goes to the clipboard, then Ctrl+V is
//! pressed through the RemoteDesktop portal (KDE has no virtual-keyboard protocol for clients).
//! The portal asks once; the grant is kept with a restore token.

use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

use anyhow::{Context, Result};
use ashpd::desktop::{
    PersistMode, Session,
    remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions, StartOptions},
};
use wl_clipboard_rs::{copy, paste};

use crate::config::{Config, state_dir};

const KEY_CONTROL_L: i32 = 0xffe3;
const KEY_SHIFT_L: i32 = 0xffe1;
const KEY_V: i32 = 0x0076;

pub struct Paster {
    portal: RemoteDesktop,
    session: Session<RemoteDesktop>,
}

fn token_path() -> std::path::PathBuf {
    state_dir().join("remote-desktop.token")
}

impl Paster {
    /// Opens a keyboard-only RemoteDesktop session (shows KDE's permission dialog the first time).
    pub async fn connect(conn: ashpd::zbus::Connection) -> Result<Self> {
        let portal = RemoteDesktop::with_connection(conn).await?;
        let session = portal.create_session(Default::default()).await?;
        let token = fs::read_to_string(token_path()).ok();
        portal
            .select_devices(
                &session,
                SelectDevicesOptions::default()
                    .set_devices(ashpd::enumflags2::BitFlags::from(DeviceType::Keyboard))
                    .set_persist_mode(PersistMode::ExplicitlyRevoked)
                    .set_restore_token(token.as_deref().map(str::trim)),
            )
            .await?
            .response()?;
        let started = portal
            .start(&session, None, StartOptions::default())
            .await?
            .response()
            .context("klavye izni verilmedi")?;
        if let Some(t) = started.restore_token() {
            let dir = state_dir();
            fs::create_dir_all(&dir)?;
            fs::write(token_path(), t)?;
            fs::set_permissions(token_path(), fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { portal, session })
    }

    async fn key(&self, keysym: i32, state: KeyState) -> Result<()> {
        self.portal.notify_keyboard_keysym(&self.session, keysym, state, Default::default()).await?;
        Ok(())
    }

    pub async fn paste(&self, cfg: &Config, text: &str) -> Result<()> {
        let previous = if cfg.restore_clipboard { read_clipboard().await } else { None };
        set_clipboard(text.to_string()).await?;
        // give the compositor a moment to announce the new selection
        tokio::time::sleep(Duration::from_millis(60)).await;
        let mut keys = vec![KEY_CONTROL_L];
        if cfg.paste_keys.to_lowercase().contains("shift") {
            keys.push(KEY_SHIFT_L);
        }
        keys.push(KEY_V);
        let mut result = Ok(());
        let mut down = Vec::new();
        for &k in &keys {
            if let Err(e) = self.key(k, KeyState::Pressed).await {
                result = Err(e);
                break;
            }
            down.push(k);
        }
        // release whatever went down, even after an error, so no modifier stays stuck
        for &k in down.iter().rev() {
            let _ = self.key(k, KeyState::Released).await;
        }
        result?;
        if let Some(prev) = previous {
            // only text is restored; the target app reads the clipboard asynchronously
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(700)).await;
                if let Err(e) = set_clipboard(prev).await {
                    log::warn!("eski pano geri konamadı: {e:#}");
                }
            });
        }
        Ok(())
    }
}

pub async fn set_clipboard(text: String) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        copy::Options::new().copy(copy::Source::Bytes(text.into_bytes().into()), copy::MimeType::Text)
    })
    .await?
    .context("panoya yazılamadı")
}

/// The current clipboard text, if it is text and answers quickly (a stuck owner must not hang us).
async fn read_clipboard() -> Option<String> {
    let read = tokio::task::spawn_blocking(|| {
        use std::io::Read;
        let (mut pipe, _) =
            paste::get_contents(paste::ClipboardType::Regular, paste::Seat::Unspecified, paste::MimeType::Text)
                .ok()?;
        let mut s = String::new();
        pipe.read_to_string(&mut s).ok()?;
        Some(s)
    });
    tokio::time::timeout(Duration::from_millis(500), read).await.ok()?.ok().flatten()
}
