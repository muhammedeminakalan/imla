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
        let shift = cfg.paste_keys.to_lowercase().contains("shift");
        self.key(KEY_CONTROL_L, KeyState::Pressed).await?;
        if shift {
            self.key(KEY_SHIFT_L, KeyState::Pressed).await?;
        }
        self.key(KEY_V, KeyState::Pressed).await?;
        self.key(KEY_V, KeyState::Released).await?;
        if shift {
            self.key(KEY_SHIFT_L, KeyState::Released).await?;
        }
        self.key(KEY_CONTROL_L, KeyState::Released).await?;
        if let Some(prev) = previous {
            // the target app reads the clipboard asynchronously; restore after it had time to
            tokio::time::sleep(Duration::from_millis(700)).await;
            set_clipboard(prev).await?;
        }
        Ok(())
    }
}

async fn set_clipboard(text: String) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        copy::Options::new().copy(copy::Source::Bytes(text.into_bytes().into()), copy::MimeType::Text)
    })
    .await?
    .context("panoya yazılamadı")
}

async fn read_clipboard() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        use std::io::Read;
        let (mut pipe, _) =
            paste::get_contents(paste::ClipboardType::Regular, paste::Seat::Unspecified, paste::MimeType::Text)
                .ok()?;
        let mut s = String::new();
        pipe.read_to_string(&mut s).ok()?;
        Some(s)
    })
    .await
    .ok()
    .flatten()
}
