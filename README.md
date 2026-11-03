# İmlâ

<p align="center"><img src="docs/hud.gif" alt="The HUD while listening, transcribing and finishing" width="504"></p>

*imlâ* (Arabic/Ottoman إملاء): dictation, writing down what is spoken.

Hold a shortcut, speak, release: what you said is typed into whatever window has the focus.
Transcription runs either in the cloud through OpenRouter or fully offline on your own machine
with a built-in Whisper model. There is no window, only a small HUD at the top of the screen while
it listens and transcribes. Built for KDE Plasma on Wayland.

## Features

- **Push-to-talk anywhere:** a global shortcut (default Ctrl+Alt+D). Hold to talk, or tap once to
  start and tap again to stop. The text is pasted into the focused app.
- **Two providers:**
  - **OpenRouter:** any audio-capable model in the cloud (default `openai/gpt-audio-mini`).
  - **Local:** whisper.cpp built in, offline. It runs on the GPU through Vulkan and falls back to
    the CPU.
- **Any language:** the speech language is detected per clip, and mixed speech is kept as spoken.
  You can also pin a language.
- **Spelling hints:** a list of names and terms (people, products, jargon) that the model should
  spell exactly.
- **HUD and tones:**
  - A dot-field HUD follows your voice, shows a light flow while transcribing and clears when done.
  - Short tones play when recording starts, stops or is dropped.
- **Polite with your clipboard:** the text you had on the clipboard before is put back after
  pasting.
- **Turkish or English interface:** it follows the session language.

## Requirements

- KDE Plasma 6 on Wayland, with `xdg-desktop-portal-kde`. It provides the global shortcut and the
  keyboard access used to paste.
- PipeWire, for `pw-record` and `pw-play`.
- For the local provider, a Vulkan driver makes it fast. Without one it uses the CPU.
- For OpenRouter, an API key from https://openrouter.ai/keys

## Install (Arch Linux)

```bash
git clone https://github.com/muhammedeminakalan/imla && cd imla
make package                                   # tests + build in Docker + makepkg
sudo pacman -U packaging/arch/imla-*-x86_64.pkg.tar.zst
imla setup
```

`make package` needs Docker and `base-devel`; no Rust or C++ toolchain is installed on the machine.

## Setup

`imla setup` asks three things, and you run it again whenever you want to change something:

1. **Provider:** pick OpenRouter or Local with the arrow keys.
2. **Key:** for OpenRouter, your API key. It is checked on the spot, and Enter keeps the saved one.
3. **Model:**
   - OpenRouter: type a model id from https://openrouter.ai/models?input_modalities=audio
   - Local: pick a model from the list. It is downloaded once into `~/.cache/imla/models`.

Then it asks for optional spelling hints, enables the background service and starts it. The
first time, KDE asks you to confirm the shortcut and to allow keyboard control (for pasting).

Options: `--provider openrouter|local`, `--model ID`, `--shortcut "CTRL+ALT+D"`,
`--language auto|tr|en|…`, `--terminal-paste` (paste with Ctrl+Shift+V).

## Use

- **Hold** the shortcut, speak, release: the text appears where your cursor is.
- **Tap** the shortcut to start a long dictation, and tap again when you are done.
- Recording stops by itself after two minutes. A silent or very short clip is dropped.
- `imla status` shows the state, the provider and the model, for example
  `ready · openrouter · openai/gpt-audio-mini`.

## Providers

| | OpenRouter | Local |
|---|---|---|
| Runs | in the cloud | on this machine, offline |
| Needs | an API key | a one-time model download |
| Models | any OpenRouter model with audio input | `large-v3-turbo` (547 MB, default), `large-v3-turbo-q8` (833 MB), `medium` (514 MB), `small` (181 MB), `base` (56 MB) |
| Speed (27 s of speech) | ~4 s | 0.9 s on an RTX 5060 Ti (Vulkan), ~13 s on the CPU |
| Cost | ~$0.06 per hour of speech with `gpt-audio-mini` | free |

In a test with Turkish speech full of numbers, names and technical terms, the cloud models were
the most accurate: `gpt-audio-mini` had a 6 % word error rate and `gemini-3.8-flash` 4 %. Local
Whisper had about 20 %, and spelling hints fix most of the names and terms it gets wrong.

## Privacy

- With **Local**, the audio never leaves the machine.
- With **OpenRouter**, each clip is sent to OpenRouter and the model provider you picked.
- The last clip is kept locally at `~/.local/state/imla/last.wav` (owner-only, overwritten each
  time) so a bad result can be checked.
- The config file holding the key is owner-only.

## Configuration

`~/.config/imla/config.toml` (mode 0600). `imla setup` writes it; you can also edit it and run
`systemctl --user restart imla`.

```toml
provider = "openrouter"            # or "local"
language = "auto"                  # or an ISO 639-1 code: "tr", "en", "de", …
terms = ["Gülendam Afra", "JSON"]  # spelling hints
shortcut = "CTRL+ALT+D"
paste_keys = "ctrl+v"              # "ctrl+shift+v" for terminals
restore_clipboard = true
sound_effects = true
sound_volume = 0.5
max_seconds = 120

[openrouter]
api_key = "sk-or-…"                # empty: taken from OPENROUTER_API_KEY
model = "openai/gpt-audio-mini"

[local]
model = "large-v3-turbo"
```

## Troubleshooting

- **Nothing happens on the shortcut:** run `imla status`, then look at the logs with
  `journalctl --user -u imla -f`. The shortcut can be changed under System Settings → Shortcuts.
- **The text is not pasted, only copied:** keyboard control was not allowed. Run `imla setup` again
  and allow it when KDE asks.
- **Pasting into a terminal does nothing:** use `imla setup --terminal-paste`.
- **Wrong or odd transcript:** listen to `~/.local/state/imla/last.wav`. Add names to `terms`, or
  try another model.
- **Remove:** `sudo pacman -R imla`. Settings stay in `~/.config/imla`, models in
  `~/.cache/imla`.

## How it works

- **Shortcut:** the XDG GlobalShortcuts portal sends press and release events. The process
  registers as `dev.akalan.imla` (host app registry and a `.desktop` entry), so KDE remembers the
  binding.
- **Audio:** `pw-record --raw`, 16 kHz mono s16, with a live RMS level for the HUD.
- **OpenRouter:** `chat/completions` with an `input_audio` WAV. A strict system prompt keeps the
  model transcribing instead of answering, and carries the spelling hints.
- **Local:** whisper-rs (whisper.cpp) with the `vulkan` feature. The model is loaded once at
  service start, and the spelling hints become the initial prompt.
- **Paste:** the text goes to the clipboard (wl-clipboard-rs). Then Ctrl+V is sent through the
  RemoteDesktop portal, keyboard only. The grant is kept with a restore token in
  `~/.local/state/imla/`.
- **HUD:** a click-through wlr layer-shell overlay on its own thread, rendered on the CPU at
  ~60 fps while visible and idle when hidden (`src/hud/`).
- **Service:** the systemd user unit `imla.service` runs `imla daemon`. A Unix socket answers
  `imla status`.

## Development

```bash
./build.sh                 # cargo build --release inside packaging/docker (Rust, cmake, clang, Vulkan headers)
./build.sh test --release  # unit tests
make package               # Arch package in packaging/arch/
```

- The version is `version` in Cargo.toml, which equals `pkgver` in the PKGBUILD and the git tag
  `vX.Y.Z`.
- Hidden commands for development:
  - `daemon`: what the service runs.
  - `toggle`: start or stop without the shortcut.
  - `transcribe file.wav`: transcribe a file.
  - `hud-demo`: show the HUD.
  - `render-demo out.rgb`: render HUD frames offscreen; `docs/hud.gif` is made from them with
    ffmpeg.

## License

MIT, see `LICENSE`. Third-party notices: `THIRD_PARTY_NOTICES.md`.
