# İmlâ

<p align="center"><img src="docs/hud.gif" alt="The HUD while listening, transcribing and finishing" width="504"></p>

*imlâ* (Arabic/Ottoman إملاء): dictation, writing down what is spoken.

Hold a shortcut, speak, release: the speech is transcribed by OpenRouter or by a local Whisper
model (offline, GPU) and pasted into the focused window. No window, only a small HUD
pill at the top of the screen: "Dot scope" (dots lit by braided traces that follow the voice,
flowing light while transcribing). Built for KDE Plasma on Wayland. Third-party licenses:
`THIRD_PARTY_NOTICES.md`.

## How it works

- **Shortcut:** XDG GlobalShortcuts portal (`Activated`/`Deactivated` = press/release). The
  process registers as `dev.akalan.imla` (host app registry + `.desktop` entry) so KDE can
  bind and remember it. Hold = record while held; a short tap latches recording until the next tap.
- **Audio:** `pw-record` (PipeWire), 16 kHz mono s16, live RMS for the meter; silent or <0.33 s
  clips are dropped; recording stops itself after `max_seconds`.
- **Transcription:** OpenRouter `chat/completions` with an `input_audio` WAV and a verbatim prompt;
  `terms` from the config are added as spelling hints (fixes names, e.g. "Gülendam Afra").
- **Paste:** text goes to the clipboard (wl-clipboard-rs, data-control), then Ctrl+V (or
  Ctrl+Shift+V) is pressed through the RemoteDesktop portal (keyboard only; KDE asks once, the
  grant is kept with a restore token in `~/.local/state/imla/`). The previous clipboard text is
  restored afterwards. Without the portal grant the text is only left on the clipboard.
- **HUD:** wlr layer-shell overlay on its own thread; click-through. Dot scope (`src/hud/`), shader evaluated on the CPU into wl_shm at ~60 fps while visible, idle when hidden. `HudMsg::Level` carries linear RMS.

## Build (no Rust toolchain on the machine; Docker)

```bash
./build.sh                 # cargo build --release in packaging/docker (Rust, cmake, clang, Vulkan headers)
./build.sh test --release  # unit tests
```

The binary links glibc, libstdc++ and libvulkan (`target/release/imla`).

## Arch package

```bash
make package                                  # tests, Docker build, dist/, makepkg
sudo pacman -U packaging/arch/imla-*-x86_64.pkg.tar.zst
imla setup                                 # once per user: key, terms; enables the user service
```

The package ships `/usr/bin/imla`, the user unit `imla.service` and the
`.desktop` entry. `setup` enables the packaged unit. Version: `pkgver` in
the PKGBUILD = `version` in Cargo.toml = git tag `vX.Y.Z`.

## Providers: OpenRouter or local

`imla setup` and `imla model` walk through it with arrow-key menus: provider (OpenRouter or local)
→ API key (OpenRouter; Enter keeps the saved one) → model. OpenRouter's list is fetched live
(audio-in, text-out models; the ones that tested best first, `:batch` variants left out, or type
any id); local models show their size and whether they are downloaded. The service restarts with
the choice.

- **OpenRouter:** an audio-capable model in the cloud (default `openai/gpt-audio-mini`); the key is
  checked at setup. Terms go into the system prompt as spelling hints.
- **local:** whisper.cpp built in (whisper-rs, Vulkan GPU, falls back to the CPU), fully offline.
  The model is downloaded once from `huggingface.co/ggerganov/whisper.cpp` into
  `~/.cache/imla/models`: `large-v3-turbo` (q5_0, 547 MB, default), `large-v3-turbo-q8` (833 MB),
  `medium` (514 MB), `small` (181 MB), `base` (56 MB). It is loaded once when the service starts.
  Terms become the initial prompt. Measured here (RTX 5060 Ti): 27 s of speech in 0.9 s on the
  GPU, ~13 s on the CPU.

```toml
provider = "openrouter"   # or "local"

[openrouter]
api_key = "sk-or-…"        # empty: OPENROUTER_API_KEY
model = "openai/gpt-audio-mini"

[local]
model = "large-v3-turbo"
```

## Languages

- Speech: any language the model understands; `language = "auto"` (default) detects it per clip.
- Interface (CLI help, messages, logs): Turkish when the session locale (`LC_ALL`/`LC_MESSAGES`/`LANG`) is Turkish, English otherwise (`src/i18n.rs`, `t()` / `tf!`).

## Commands

- `imla setup [--provider openrouter|local] [--model ID] [--shortcut KEYS] [--language xx] [--terminal-paste]`: provider, key or local model (downloaded), terms; enables and restarts the service
- `imla model`: choose the provider and model again (menus; the service restarts itself)
- `imla status`: ready / listening / transcribing, or not running
- Hidden: `daemon` (run by systemd), `toggle` (start/stop without the shortcut), `transcribe x.wav`, `hud-demo`, `render-demo out.rgb` (offscreen HUD frames; `docs/hud.gif` is made from it with ffmpeg)
- Logs: `journalctl --user -u imla -f` · Remove: `sudo pacman -R imla`

## Config (`~/.config/imla/config.toml`, 0600)

`provider`, `[openrouter]`, `[local]` (above), `sound_effects` / `sound_volume` (start/stop/cancel tones via `pw-play`, default on, 0.5), `language` (`auto` = written in whatever language is spoken, mixed speech kept as spoken; or an ISO 639-1 code such as `tr`, `en`, `de`; `imla setup --language xx`), `terms` (list), `shortcut`, `paste_keys`
(`ctrl+v` / `ctrl+shift+v`), `restore_clipboard`, `max_seconds`. `OPENROUTER_API_KEY` in the
environment is used when `api_key` is empty.
