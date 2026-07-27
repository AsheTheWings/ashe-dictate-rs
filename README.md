# Ashe Worker

Ashe Worker is a native Windows productivity companion for dictation, selected-text
actions, and an automatic computer-activity journal. It runs in the system tray and uses
small overlays to show live state and results.

## Capabilities

- Dictate into the active text field, with speech transcription and automatic cleanup.
- Correct grammar, spelling, and punctuation in selected text.
- Ask a question using selected text and append the answer in place.
- Capture desktop activity on an adaptive cadence and generate grounded block reports.
- Slow capture during idle periods, stop while Windows is locked, and omit sensitive apps.
- Maintain recent detailed context, a bounded rolling summary, and a complete daily report.
- Store each completed activity block as canonical JSON while aggregating its report into
  the human-readable journal.
- Seal older day artifacts into authenticated, self-contained encrypted archives and
  optionally send those archives to an authenticated remote endpoint.

## Hotkeys

| Hotkey | Action |
| --- | --- |
| `Win+Shift+H` | Start or stop dictation |
| `Win+Shift+G` | Correct the selected text |
| `Win+Shift+Q` | Answer the selected question |

While dictating, press `Enter` to finish, `Esc` to cancel, `Backspace` to remove the last
sentence, or `Shift+Backspace` to clear the transcript.

## Setup

Use [.env.example](.env.example) as the complete configuration reference. Copy it to the
local environment file beside the executable or at the project root, then provide the
credentials and settings needed for the features you enable. Secret values are not logged.

The app starts in the tray. Left-click the tray icon to open the artifact collection.
Right-click it to control dictation and journaling, reload configuration, open logs, or
quit.

## Activity journal

Each block combines measured foreground-window and idle-time telemetry with selected
lossless screenshots. Visually redundant frames are omitted from model requests, request
count and payload size are capped, and blocks with too little active or visual change are
stored without an unnecessary model call.

The model returns a title, Markdown report, and a list of subjects. Each subject includes
one to four broad-to-specific namespaces and an independently estimated duration, so
overlapping estimates remain valid during multitasking. Parseable generated data is kept
without semantic filtering. The chronological `journal.md` is rebuilt from the report
fields in the JSON block artifacts.

Recent completed blocks remain available in full. Older blocks are folded once into the
bounded plaintext `summary.md`, which serves as convenient long-term context. At the end of
each day, all block reports are also assembled into a full daily report with measured totals
and explicit coverage gaps.

## Encrypted archives

The current and previous local calendar days remain directly accessible. Once a day is old
enough, has no pending blocks, and has a current daily report when daily reports are enabled,
its non-image artifacts are packed into one `archive.ashe`. The worker verifies the archive
before deleting the plaintext artifacts, raw frames, and retained keyframes. `summary.md`
always remains accessible.

Every archive uses a random data key with Libsodium secretstream XChaCha20-Poly1305. The
data key is wrapped to the archive recipient, and the archive embeds the private-key envelope
protected by the five-word passphrase. An archive and that passphrase are sufficient for
recovery; losing the passphrase permanently loses access by design.

To restore an archive, run the supplied decrypt utility with the archive and a new output
directory, then enter the passphrase at its hidden prompt:

```powershell
.\ashe-archive-decrypt.exe <archive> <output-directory>
```

Remote backup is an abstract authenticated HTTPS upload. The worker sends only the encrypted
archive and relies on a successful acknowledgement for retry-safe delivery; it has no
knowledge of the receiver's storage or replication implementation.

## Build and release

Build the Windows binary from the project root:

```powershell
cargo build --release --target x86_64-pc-windows-gnu
```

From Linux or WSL, the shipping script cross-builds the worker and decrypt utility, then
copies them to the configured release directory:

```bash
./scripts/ship-windows-release.sh
```

The script requires the Windows GNU Rust target, the MinGW cross-compiler, an encrypted
recipient input, and an explicit release directory in the local environment file. It aborts
without those paths and has no built-in release destination.

## Logs

The worker writes a local log beside the executable. Open it from the tray menu when a
hotkey, microphone, network request, capture, report, archive, or upload fails.
