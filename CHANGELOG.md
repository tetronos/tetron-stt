# Changelog

## [0.15.2]

- The TUI is tetron-wm's dictation engine: inside tetron-wm it reports its state (`tetron-wm dictation-state`) so the tray mic follows it; tetron-wm opens and toggles it.
- Memory: audio is kept only in final mode; a recorder faster than the engine is capped at 5 s of backlog (audio dropped with a note) instead of queueing without bound; final mode commits a long utterance in pieces (at a pause past 60 s, at 90 s at the latest) and keeps listening; the final model loads only for final mode and is unloaded after a stop, so its decode memory goes back to the system (90 s utterance: 2.5 GB kept before, 0.76 GB now).
- X11: the own-window check falls back to the X input focus when there is no window manager.
- TUI types into the focused window inside tetron-wm with `tetron-wm type` (no X11 needed). From the TUI's own window tetron-wm puts the text on its clipboard. X11 still uses xdotool; without either the TUI copies with OSC 52. `tetron-stt doctor` names the typer in use.

## [0.15.1]

- Release: a `.deb` for amd64 and arm64 is built in the release workflow with the shared build-deb action and attached to the release (binary only, the models are not packaged).
- Models: `/usr/share/tetron-stt/models` is used before the per-user directory when it holds every model, so a system image can ship them. `TETRON_STT_MODELS` still overrides.

## [0.15.0]

Version follows the tetron core minor (0.15), like the rest of the suite; 0.1.0 was the first release under the wrong number.

- TUI: the bottom bar is a row of mouse buttons (Start/Stop, Mode, Output, Copy last, Clear, Quit), each labelled with its key. A click acts on release over the same button; Start turns into a red Stop while listening. Quit is always shown at the right edge; on a narrow terminal the labels drop their key hints first, then Clear, Copy and Output drop (keys still work).

## [0.1.0]

First version.
- TUI: live text while speaking, history of committed text, space toggles, `m` switches segment/final, `o` picks the output (type into the focused X11 window, copy with OSC 52, show only).
- `tetron-stt toggle` and `tetron-stt mode` signal the running TUI, for global desktop shortcuts.
- `tetron-stt serve`: JSON-lines provider for tetron-wm (start/stop/toggle/mode in; state/partial/commit/error out).
- Live model Nemotron 3.5 ASR streaming 0.6B (560 ms, int8); final pass Parakeet TDT 0.6B v2 (int8); CPU only, sherpa-onnx linked statically.
- Segment commits are cut on a pause in the decoded text, not with the model's endpoint and stream reset, which dropped words still being decoded.
- Post-filter: spoken slash, underscore, dash and (before an extension, slash or dot) dot become symbols; a trailing enter word ("make it so") presses Enter.
- `models status|download|path` with `--json`; explicit, resumable download with sha256 check. `doctor`, `transcribe FILE`.
- Mic open only while listening; models load on first start and unload when idle.
