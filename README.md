# Ashe Dictate RS

A native Windows productivity app that turns speech into polished text and runs quick
text actions on whatever you have selected — all from global hotkeys, anywhere in
Windows. Speech is transcribed with Deepgram and text is processed through the tera LLM
gateway.

## What it does

- **Dictate anywhere** (`Win+Shift+H`) — speak, and the cleaned-up text is inserted into
  the active text field.
- **Fix grammar** (`Win+Shift+G`) — corrects grammar, spelling, and punctuation of the
  selected text and replaces the selection with the fixed version.
- **Ask a question** (`Win+Shift+Q`) — sends the selected text to the LLM as a question
  and appends the answer right after your selection.
- A small overlay near your cursor shows live status and results.
- Lives in the system tray with start/stop, reload config, log access, and about.

## Hotkeys

| Hotkey | Action |
| --- | --- |
| `Win+Shift+H` | Start / stop dictation |
| `Win+Shift+G` | Fix grammar of the selected text (replaces it) |
| `Win+Shift+Q` | Answer the selected text as a question (appends the answer) |

While dictating:

| Key | Action |
| --- | --- |
| `Enter` | Finish and insert |
| `Esc` | Cancel |
| `Backspace` | Remove the last sentence |
| `Shift+Backspace` | Clear everything captured so far |

## Setup

Create a `.env.local` file next to the executable (or in the project root) with your keys:

```env
DEEPGRAM_API_KEY=your_deepgram_key
TERA_API_KEY=your_tera_key
```

Optional settings (sensible defaults are used if omitted):

```env
DEEPGRAM_MODEL=nova-3
DEEPGRAM_LANGUAGE=en-US
DEEPGRAM_KEYTERMS=Rust,Win32,Deepgram,TypeScript,React
TERA_API_BASE=https://tera.asheservices.online/v1
TERA_MODEL=cloudcode/chat-gemini-3-flash-paid-tier
ASHE_OUTPUT_SAMPLE_RATE=48000
ASHE_LLM_TEMPERATURE=0.2
```

`DEEPGRAM_KEYTERMS` is a comma-separated list of terms to bias transcription toward.
API keys are never written to the log.

## Build

```powershell
cargo build --release --target x86_64-pc-windows-gnu
```

To cross-build from Linux/WSL and copy the executable into the release folder:

```bash
./scripts/ship-windows-release.sh
```

This requires the `x86_64-pc-windows-gnu` Rust target and `x86_64-w64-mingw32-gcc`.

## Run

```powershell
.\target\release\ashe-dictate-rs.exe
```

The app starts in the tray. Press a hotkey to use it:

- **Dictation:** press `Win+Shift+H`, speak into your default microphone, and press it
  again (or `Enter`) to finish. The polished text is pasted into the field you were in.
- **Fix grammar:** select some text, press `Win+Shift+G`, and the corrected text replaces
  your selection.
- **Ask a question:** select a question, press `Win+Shift+Q`, and the answer is added
  after it.

Right-click the tray icon for start/stop, config reload, log access, and about.

## Logs

The app writes a log file beside the executable (and the tray menu can open it or copy
its path). Check the log if a hotkey fails to register, a microphone or network error
occurs, or text fails to paste.
