# Changelog

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
