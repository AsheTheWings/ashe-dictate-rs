# Ashe Dictate RS

Native Windows dictation app written in Rust.

## Features

- Global hotkey: `Ctrl+Shift+D`
- Native Win32 hidden window and message loop via the `windows` crate
- System tray icon with start/stop and quit menu
- Small green overlay dot near the cursor while dictating
- Microphone capture via `cpal`
- Realtime Deepgram streaming through the Deepgram Rust SDK
- Clipboard paste injection into the active input field
- Clipboard retry and previous-text restore for Unicode clipboard text
- Non-blocking stop/shutdown flow for Deepgram finalization
- Configurable output sample rate with lightweight linear resampling
- Tray utilities for reload config, open log, copy log path, and about/status
- File logging next to the executable

## Implementation Status

The current implementation builds successfully in both debug-check and release modes:

```powershell
cargo check
cargo build --release
```

Core runtime paths are implemented with explicit lifecycle state:

- `Idle`
- `Starting`
- `Listening`
- `Stopping`

Stop requests do not block the Win32 UI thread. The app stops microphone capture immediately, asks the Deepgram worker to finalize/close, and polls worker completion from the timer pump.

## Configuration

The app reads environment variables from the current directory, parent directories, or the sibling C++ project file:

```powershell
E:\Desktop\ashe-dictate\.env.local
```

Required:

```env
DEEPGRAM_API_KEY=your_key_here
```

Optional:

```env
DEEPGRAM_MODEL=nova-3
DEEPGRAM_LANGUAGE=en-US
DEEPGRAM_KEYTERMS=Rust,Win32,Deepgram,WASAPI,TypeScript,React
ASHE_OUTPUT_SAMPLE_RATE=48000
```

The app logs a sanitized configuration summary and never logs the API key.
`ASHE_OUTPUT_SAMPLE_RATE` controls the outgoing mono `linear16` stream sent to Deepgram. If the microphone uses a different native sample rate, the app applies streaming linear resampling before transmission.

## Build

```powershell
cargo check
cargo build --release
```

The release executable is:

```powershell
E:\Desktop\ashe-dictate-rs\target\release\ashe-dictate-rs.exe
```

## Run

```powershell
.\target\release\ashe-dictate-rs.exe
```

Then press:

```text
Ctrl+Shift+D
```

Expected behavior:

- First press starts dictation.
- A green dot appears near the cursor.
- The tray tooltip changes through connecting/listening/status states.
- Right-click the tray icon for start/stop, config reload, log utilities, about, and quit.
- Speak into the default microphone.
- Final Deepgram transcripts are pasted into the active text field.
- Second press requests a clean stop.

## Logs

The app writes logs beside the executable:

```powershell
E:\Desktop\ashe-dictate-rs\target\release\ashe-dictate-rs.log
```

Useful events to verify:

- App startup and log path
- Sanitized config summary
- Overlay creation
- Hotkey pressed
- Audio input selected
- Audio capture started/stopped
- Deepgram connecting/connected
- Deepgram chunks sent
- Deepgram speech/transcript events
- Audio input/output sample-rate diagnostics
- Resampler enabled/disabled status
- Clipboard retry/restore failures, if any
- Text injection failures, if any
- Clean worker shutdown

## Error Handling

Implemented safeguards include:

- Missing `DEEPGRAM_API_KEY` validation before dictation starts
- Hotkey registration failure dialog/logging
- Timer setup failure logging
- Audio device/config/start errors surfaced through log/dialog
- Deepgram runtime/connect/send/receive/finalize/close logging
- Duplicate final transcript suppression
- Clipboard busy retry loop
- `SendInput` event-count validation
- Non-blocking Deepgram worker shutdown and join polling
- Audio bridge thread completion logging
- Config reload guarded while dictation is active
- Log file open/copy utilities with error reporting

## Notes

This implementation currently uses `cpal` for microphone capture instead of direct WASAPI. The outgoing audio is mono little-endian 16-bit linear PCM at `ASHE_OUTPUT_SAMPLE_RATE`, defaulting to `48000`.

Future parity work may include direct WASAPI capture, packaged app icon/resources, installer/startup integration, and richer persistent settings UI.
