# Ashe Worker

Ashe Worker is a tray-based Windows companion for hands-free writing, quick text assistance, and
personal activity reflection. It combines speech transcription, contextual text actions, and an
automatic journal of computer activity and learning in one lightweight desktop application.

The project is intended for people who want useful context about how they spend time at their
computer without manually maintaining a work log. Reports remain available as local artifacts,
and older records can be placed in encrypted archives for retention or backup.

## Features

- Dictate into the active application.
- Correct selected text or ask a question about it.
- Create activity, learning, and daily reports automatically.
- Keep a rolling summary and chronological journal.
- Exclude configured applications from activity capture.
- Encrypt older records and optionally upload the encrypted archives.
- Upload a clipboard image and paste its remote path into the active application.

## Getting started

1. Build or obtain `ashe-worker.exe` and place it in a directory where it can keep its local
   configuration and artifacts.
2. Copy [.env.example](.env.example) to `.env` beside the executable or at the project root.
3. Add the credentials for the features you want to use and review the optional paths, privacy
   exclusions, per-feature LLM models, reporting, and archive settings.
4. Launch `ashe-worker.exe`.

The app starts in the Windows system tray. Left-click the tray icon to open the artifact
collection. Right-click it to control features, reload configuration, open logs, or quit.

## Hotkeys

| Hotkey | Action |
| --- | --- |
| `Win+Shift+H` | Start or stop dictation |
| `Win+Shift+G` | Correct selected text |
| `Win+Shift+Q` | Ask a question using selected text |
| `Ctrl+Alt+V` | Upload a clipboard image and paste its remote path |

During dictation, microphone audio is buffered locally while the pill overlay
shows a live voice spectrum (FFT frequency bands) with a cyan border.
Transcription runs once when you
stop, as a single full-utterance request for a better result than streaming
partials; while it works, the pill shows `processing...`. Speech-to-text runs
through the fal.ai queue API (`FAL_STT_MODEL`, default scribe-v2): the worker
embeds the utterance as a WAV data URI, polls the request to completion, and
inserts the transcript, so dictation needs `FAL_KEY` set. State and errors are
also reported through the tray tooltip and the log. Use `Enter` to finish and
transcribe, or `Esc` to cancel.

## Activity records

When enabled, activity tracking periodically observes the configured display and produces local
reports describing meaningful activity and learning. The artifact collection includes recent
reports, a chronological journal, a rolling summary, and daily overviews.

Review [.env.example](.env.example) to choose where records are stored, disable tracking, exclude
sensitive applications, or configure retention and encrypted backup. Activity capture pauses while
Windows is locked.

## Encrypted archives

The companion CLI can manage encrypted activity archives:

```powershell
.\ashe-worker-cli.exe archive seal <YYYY-MM-DD>
.\ashe-worker-cli.exe archive upload <archive.ashe>
.\ashe-worker-cli.exe archive decrypt <archive.ashe> <output-directory>
```

Archive recovery requires the passphrase created when the archive is sealed. Store that passphrase
safely; it cannot be recovered by the application.

## Building

Build the Windows application and CLI with Cargo:

```powershell
cargo build --release
```

Linux and WSL users can use the included cross-build helper after configuring the release settings
described in [.env.example](.env.example):

```bash
./scripts/ship-windows-release.sh
```

The helper validates the runtime `.env.local` already present in `ASHE_RELEASE_DIR`; it does not
copy the project `.env.local` or secrets into the release directory.

## Troubleshooting

Open the local log from the tray menu when a hotkey, microphone, network request, activity report,
or archive operation fails.
