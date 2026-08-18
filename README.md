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
   exclusions, reporting, and archive settings.
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

During dictation, use `Enter` to finish, `Esc` to cancel, `Backspace` to remove the latest
sentence, or `Shift+Backspace` to clear the transcript.

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
