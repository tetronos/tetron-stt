# tetron-stt

Local speech-to-text dictation that runs on the CPU. No GPU, no cloud, no account. You speak, the text appears live, and it is typed into the window you are working in, for example a coding agent CLI (Claude Code, opencode, pi), a shell or an editor.

Part of the tetron suite, and useful on its own:

- **TUI**: run `tetron-stt` in any terminal. It shows the live text as you speak and a history of what was committed, and toggles with the space bar.
- **Global hotkey**: bind `tetron-stt toggle` and `tetron-stt mode` to desktop shortcuts and dictate into any focused window.
- **Dictation of tetron-wm**: the TUI is the engine. A click on the tray mic (or the dictation key) opens it listening in the background, or toggles it when it runs; right-click for Show, Start/Stop, Mode and Settings. The TUI types into the focused window with `tetron-wm type` and reports its state to the tray (tetron-wm 0.15.24 or later).
- **Provider protocol**: `tetron-stt serve` speaks JSON lines on stdin/stdout, for other programs (and tetron-wm with a custom `dictation_command`).

## How it works

Two models run with [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx), linked statically into the binary:

| role | model | size | speed on a laptop CPU, 4 threads |
|---|---|---|---|
| live | NVIDIA Nemotron 3.5 ASR streaming 0.6B, 560 ms chunks, int8 | 475 MB | 0.4 x real time |
| final | NVIDIA Parakeet TDT 0.6B v2, int8 | 482 MB | about 1 s for 12 s of speech |

There are two modes, switched with `m` in the TUI, `tetron-stt mode` or `--mode`:

- **segment** (default): the live model's text is committed at every pause, so words land in the target window while you talk.
- **final**: the text is shown live but not committed. On stop, the final model re-transcribes the whole utterance once. This is more accurate and arrives about a second after you stop.

Post-filter for developers:
- **Spoken symbols:** "slash" becomes `/`, "underscore" becomes `_`, "dash" or "hyphen" becomes `-`, and "dot" becomes `.` before a file extension, a slash or another dot. For example, "src slash main dot rs" becomes `src/main.rs`.
- **Enter word:** ending an utterance with the enter word, "make it so" by default, presses Enter. A rare phrase is used on purpose so that normal speech never submits a prompt.

## Install

Prebuilt binaries for Linux (x86_64, aarch64) and macOS (Apple Silicon, Intel) are on the [releases page](https://github.com/tetronos/tetron-stt/releases):

```sh
tar xzf tetron-stt-x86_64-unknown-linux-gnu.tar.gz
install tetron-stt ~/.local/bin/
tetron-stt models download   # ~0.96 GB, once, resumable, sha256 checked
tetron-stt doctor
```

On Debian, Ubuntu and TetronOS the release also has a `.deb` (amd64, arm64), and the tetronos apt repository carries it once listed there. It holds the binary only, no models: run `tetron-stt models download` after installing, or use a system directory (below).

```sh
sudo apt install ./tetron-stt_0.15.1-1_amd64.deb
tetron-stt models download
```

**System models directory:** if `/usr/share/tetron-stt/models` holds every model, `tetron-stt` uses it before the per-user directory (the TetronOS Desktop image pre-seeds it, so dictation works offline). `TETRON_STT_MODELS` still overrides both. To fill it: `sudo TETRON_STT_MODELS=/usr/share/tetron-stt/models tetron-stt models download`.

From source: `cargo install --git https://github.com/tetronos/tetron-stt`.

Runtime tools:
- **Recorder:** one of `arecord` (alsa-utils), `pw-record` (PipeWire) or `rec` (sox).
- **Typing into the focused window:** inside tetron-wm, `tetron-wm type` (tetron-wm 0.15.24 or later, no X11 needed); on X11, `xdotool`. Without either, the TUI copies committed text to the clipboard with OSC 52.
- **Optional:** `ffmpeg`, for `--file` and `transcribe`.

## Use

```sh
tetron-stt                      # TUI
tetron-stt --mode final         # start in final mode
tetron-stt toggle               # from a desktop shortcut: toggle the running TUI
tetron-stt mode                 # from a desktop shortcut: switch segment/final
tetron-stt transcribe talk.wav  # transcript of a file
tetron-stt --help
```

TUI keys (each is also a clickable button in the bottom bar; a click acts on release over the same button):

| key | action |
|---|---|
| space, Enter | toggle listening |
| m | switch mode |
| o | change the output: type into the focused window, copy to the clipboard, or show only |
| c | copy the last text |
| x | clear the history |
| q, Esc | quit |

When the output is "type", text goes to whichever window has focus. Start listening, then click into the target window. If the TUI's own window has focus, the text is copied to the clipboard instead of being typed into the TUI.

Example: GNOME/Cinnamon shortcuts (X11):

```sh
gsettings set org.cinnamon.desktop.keybindings custom-list "['custom0','custom1']"
# custom0: Super+Alt+V runs `tetron-stt toggle`, custom1: Super+Alt+M runs `tetron-stt mode`
```

## Provider protocol (`tetron-stt serve`)

One JSON object per line.

| direction | message |
|---|---|
| to tetron-stt | `{"cmd":"start"}` `{"cmd":"stop"}` `{"cmd":"toggle"}` `{"cmd":"mode"}` (switch) `{"cmd":"mode","mode":"final"}` `{"cmd":"quit"}` |
| from tetron-stt | `{"state":"idle\|loading\|listening\|finishing","mode":"segment"}` `{"partial":"text so far"}` `{"commit":"text","enter":false}` `{"error":"..."}` `{"info":"..."}` |

- **Commits:** paste each commit as is. Segment commits carry their own trailing space. Press Enter when `enter` is true.
- **Mic:** open only while listening.
- **Models:** load on the first start and unload after `--idle-unload` seconds idle (default 600).
- **Exit:** EOF on stdin quits.

`tetron-stt models status --json` and `tetron-stt models download --json` print JSON lines for a settings UI. Progress lines look like `{"model":...,"bytes":...,"total":...}`, followed by `{"installed":...}` per model and `{"done":true}` at the end.

## Options

| option | default | |
|---|---|---|
| `--mode segment\|final` | segment | starting mode |
| `--threads N` | min(4, cores) | CPU threads per model |
| `--pause S` | 1.2 | silence that ends a segment |
| `--enter-word WORDS` | make it so | trailing phrase that presses Enter; `""` disables |
| `--idle-unload S` | 600 | free the models after this long idle; 0 keeps them |
| `--file WAV` | | play a file in real time instead of the mic (tests, demos) |
| `--start` | | start listening at launch |

Environment: `TETRON_STT_MODELS` (models directory, default `~/.local/share/tetron-stt/models`), `TETRON_STT_AUDIO_CMD` (recorder writing raw s16le mono 16 kHz to stdout), `TETRON_STT_DEBUG` (trace live decoding to stderr).

## License

tetron-stt is MPL-2.0. The models are NVIDIA's, converted to ONNX by the [sherpa-onnx](https://k2-fsa.github.io/sherpa/onnx/) project, and are not bundled. Their licenses are on the model cards: [Nemotron 3.5 ASR streaming](https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b) and [Parakeet TDT 0.6B v2](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2) (CC-BY-4.0).
