# Ashe Worker

A native Windows productivity worker for dictation, selected-text actions, and an
automatic computer-activity journal. Speech is transcribed with Deepgram; text and
screen-journal entries are processed through the Tera Responses API.

## What it does

- **Dictate anywhere** (`Win+Shift+H`) — speak, and the cleaned-up text is inserted into
  the active text field.
- **Fix grammar** (`Win+Shift+G`) — corrects grammar, spelling, and punctuation of the
  selected text and replaces the selection with the fixed version.
- **Ask a question** (`Win+Shift+Q`) — sends the selected text to the LLM as a question
  and appends the answer right after your selection.
- **Keep an activity journal** — captures lossless desktop images on a cadence, combines
  them with measured foreground-window and idle-time data, and writes grounded Markdown
  reports. Static and idle periods avoid unnecessary LLM calls.
- A small overlay near your cursor shows live status and results.
- Lives in the system tray with dictation, journal pause/resume, artifact access, config,
  log, and about actions.

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

Copy `.env.example` to `.env.local` next to the executable (or in the project root), then
fill in the secret values:

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

# Activity journal
ASHE_JOURNAL_ENABLED=true
ASHE_ARTIFACTS_DIR=C:\path\to\artifacts
ASHE_CAPTURE_INTERVAL=20
ASHE_IDLE_CAPTURE_INTERVAL=120
ASHE_TELEMETRY_INTERVAL=2
ASHE_BLOCK_MINUTES=10
ASHE_MONITOR=1
ASHE_MAX_FRAME_GAP_S=120
ASHE_MAX_FRAMES_PER_CALL=40
ASHE_MAX_PAYLOAD_MB=48
ASHE_IDLE_THRESHOLD_S=120
ASHE_MIN_ACTIVE_SECONDS=30
ASHE_DENYLIST=1Password;Bitwarden;Private browsing
ASHE_FRAME_RETENTION_MINUTES=30
ASHE_CONTEXT_BLOCS=6
ASHE_MAX_SUMMARY_CHARS=1500
ASHE_DAILY_REPORT_ENABLED=true
ASHE_DAILY_REPORT_GRACE_MINUTES=15
```

`DEEPGRAM_KEYTERMS` is comma-separated; `ASHE_DENYLIST` is semicolon-separated. The
journal also accepts `ASHE_API_KEY`, `ASHE_BASE_URL`, and `ASHE_MODEL` as aliases for the
Tera settings. API keys are never written to the log.

After `ASHE_IDLE_THRESHOLD_S` without keyboard or mouse input, capture slows to
`ASHE_IDLE_CAPTURE_INTERVAL`. Capture stops while the workstation is locked and resumes
immediately after unlock. A block is stored without an LLM description only when it has
less than `ASHE_MIN_ACTIVE_SECONDS` of input activity and no visual change beyond the
first frame.

The journal context is a non-overlapping chronological sequence. At most the
`ASHE_CONTEXT_BLOCS` newest successful reports are supplied in full, oldest first and
without character truncation; every earlier report is folded into the persistent rolling
summary exactly once. Each complete recent report includes its exact start and end window.
The rolling summary is capped by `ASHE_MAX_SUMMARY_CHARS`. Increasing
`ASHE_CONTEXT_BLOCS` does not pull reports back out after they have already entered the
summary.

After a local day closes and the grace period elapses, a separate background worker
sends every stored block report for that day to the LLM in one chronological,
untruncated call. It adds code-calculated totals and explicit pending or unknown coverage
gaps, then atomically writes `daily.md`. The source hash makes the operation idempotent
and causes a report to be regenerated if a late block changes the day's inputs. Daily
reports are for human consumption and are never fed back into block context.

Each completed block is stored as canonical JSON containing worker-measured timing,
application, frame, and timeline data together with the model-generated `title`, Markdown
`report`, and structured `subjects`. Each subject has one to four ordered, broad-to-specific
lowercase kebab-case namespaces, one factual subject statement, and an
`estimated_duration_s`. Subjects may overlap when the user multitasks, so their estimates
are independent and do not need to sum to measured coverage. These are generation
instructions, not ingestion checks: parseable model output is preserved for optional later
analysis or filtering. The provider is prompted to return the JSON object directly. One
exact outer JSON Markdown fence is tolerated as transport formatting; arbitrary surrounding
text is not. Output becomes a terminal `invalid_model_output` block only when it cannot be
parsed into the required JSON fields, without semantic repair or retry.

`journal.md` remains the chronological human-readable block journal and is aggregated from
the JSON blocks' `report` fields. The rolling `summary.md` and end-of-day `daily.md` remain
Markdown.

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
.\target\release\ashe-worker.exe
```

The app starts in the tray. Press a hotkey to use it:

- **Dictation:** press `Win+Shift+H`, speak into your default microphone, and press it
  again (or `Enter`) to finish. The polished text is pasted into the field you were in.
- **Fix grammar:** select some text, press `Win+Shift+G`, and the corrected text replaces
  your selection.
- **Ask a question:** select a question, press `Win+Shift+Q`, and the answer is added
  after it.

Left-click the tray icon to open the artifacts root—the stable home for journal output
and future Ashe Worker features. Right-click it to start or stop dictation, pause or
resume the activity journal, open the artifacts folder or today's journal, reload
configuration, inspect the log, or quit. The icon used by both the executable and tray
is `assets/ashe-worker.png` (packaged as `assets/ashe-worker.ico` for Windows).

## Activity journal artifacts

Artifacts default to an `artifacts` folder beside the executable:

```text
artifacts/
  context-state.json           high-water mark preventing context overlap
  summary.md                   rolling compressed history (1,500 chars by default)
  pending/                    crash-safe in-progress block manifests
  YYYY-MM-DD/
    journal.md                chronological reports aggregated from block JSON
    daily.md                  full-day human report generated in one LLM call
    blocks/HHMM-HHMM.json     canonical metrics, report and subjects for each completed block
    frames/*.webp             temporary lossless captures
    keyframes/*.webp          one retained image per described block
```

Pending blocks resume after a restart. Visually unchanged images are removed from model
requests, request count and payload are capped, sensitive windows can be excluded with
`ASHE_DENYLIST`, and processed frames are purged after the configured retention period.

## Logs

The app writes a log file beside the executable (and the tray menu can open it or copy
its path). Check the log if a hotkey fails to register, a microphone or network error
occurs, or text fails to paste.
