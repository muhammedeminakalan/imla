# dictate

Hold a shortcut, speak, release: the speech is transcribed by an audio model on OpenRouter
(default `openai/gpt-audio-mini`) and pasted into the focused window. No window, only a small HUD
pill at the top of the screen: "Dot scope" (dots lit by braided traces that follow the voice,
flowing light while transcribing). Built for KDE Plasma on Wayland. Third-party licenses:
`THIRD_PARTY_NOTICES.md`.

## How it works

- **Shortcut:** XDG GlobalShortcuts portal (`Activated`/`Deactivated` = press/release). The
  process registers as `dev.akalan.dictate` (host app registry + `.desktop` entry) so KDE can
  bind and remember it. Hold = record while held; a short tap latches recording until the next tap.
- **Audio:** `pw-record` (PipeWire), 16 kHz mono s16, live RMS for the meter; silent or <0.33 s
  clips are dropped; recording stops itself after `max_seconds`.
- **Transcription:** OpenRouter `chat/completions` with an `input_audio` WAV and a verbatim prompt;
  `terms` from the config are added as spelling hints (fixes names, e.g. "Gülendam Afra").
- **Paste:** text goes to the clipboard (wl-clipboard-rs, data-control), then Ctrl+V (or
  Ctrl+Shift+V) is pressed through the RemoteDesktop portal (keyboard only; KDE asks once, the
  grant is kept with a restore token in `~/.local/state/dictate/`). The previous clipboard text is
  restored afterwards. Without the portal grant the text is only left on the clipboard.
- **HUD:** wlr layer-shell overlay on its own thread; click-through. Dot scope (`src/hud/`), shader evaluated on the CPU into wl_shm at ~60 fps while visible, idle when hidden. `HudMsg::Level` carries linear RMS.

## Build (no Rust toolchain on the machine; Docker)

```bash
./build.sh                 # cargo build --release in rust:1-slim-bookworm
./build.sh test --release  # unit tests
```

The binary only links glibc (`target/release/dictate`).

## Arch package

```bash
make package                                  # tests, Docker build, dist/, makepkg
sudo pacman -U packaging/arch/dictate-*-x86_64.pkg.tar.zst
dictate setup                                 # once per user: key, terms; enables the user service
```

The package ships `/usr/bin/dictate`, the user unit `app-dev.akalan.dictate.service` and the
`.desktop` entry. `setup` enables the packaged unit. Version: `pkgver` in
the PKGBUILD = `version` in Cargo.toml = git tag `vX.Y.Z`.

## Commands

- `dictate setup [--shortcut KEYS] [--terminal-paste]`: key, terms; enables and restarts the service
- `dictate status`: ready / listening / transcribing, or not running
- Hidden: `daemon` (run by systemd), `toggle` (start/stop without the shortcut), `transcribe x.wav`, `hud-demo`
- Logs: `journalctl --user -u app-dev.akalan.dictate -f` · Remove: `sudo pacman -R dictate`

## Config (`~/.config/dictate/config.toml`, 0600)

`api_key`, `model`, `sound_effects` / `sound_volume` (start/stop/cancel tones via `pw-play`, default on, 0.5), `language` (`tr`), `terms` (list), `shortcut`, `paste_keys`
(`ctrl+v` / `ctrl+shift+v`), `restore_clipboard`, `max_seconds`. `OPENROUTER_API_KEY` in the
environment is used when `api_key` is empty.
