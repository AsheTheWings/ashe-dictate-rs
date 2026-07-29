# Ashe Worker

Ashe Worker is a native Windows productivity companion for dictation, selected-text
actions, and automatic computer-activity tracking. It runs in the system tray and uses
small overlays to show live state and results.

## Capabilities

- Dictate into the active text field, with speech transcription and automatic cleanup.
- Correct grammar, spelling, and punctuation in selected text.
- Ask a question using selected text and append the answer in place.
- Capture desktop activity every 10 seconds by default and generate grounded block reports.
- Continue evidence capture while input-idle, stop while Windows is locked, and omit sensitive
  apps.
- Maintain recent detailed context, a bounded rolling summary, and a complete daily report.
- Store each completed activity block as canonical JSON while aggregating its report into
  the human-readable `journal.md` projection.
- Generate activity and learning together, including atomic learning topics, visible searches
  and sources, and an evidence-backed treatment depth.
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
Right-click it to control dictation and activity tracking, reload configuration, open logs, or
quit.

## Activity tracking

Each block combines measured foreground-window and idle-time telemetry with selected lossless
screenshots captured every 10 seconds by default while the workstation is unlocked. Visually
redundant frames are omitted from model requests, request count and payload size are capped, and
blocks with too little active or visual change are stored without an unnecessary model call.

One model request returns a title, Markdown report, broad activity subjects, and independently
validated learning subjects. Each activity subject includes one to four broad-to-specific
namespaces, an independently estimated duration, and an `unattended` judgment. Estimates remain
valid when they overlap during multitasking. Namespaces use stable domain-specific positions;
preferred roots include `software-development`, `entertainment`, `social-media`, and `learning`.

Agentic development receives a distinct `software-development/agentic-coding` activity subject
when supported. That subject records an `agentic_coding` object containing the visible medium
(`code-editor` or `tui`), a canonical tool name such as `cursor`, `devin`, `zed`, `vscode`,
`opencode`, or `codex`, and `model_id` only when the identifier is visible. The metadata is
subject-scoped so one block can accurately represent multiple coding environments.

Within the same response, the model independently validates whether evidence contains transferable
intellectual content rather than merely information retrieval or task execution. A genuine unit
identifies a concept, mechanism, relationship, rationale, method, argument, or principle that was
substantively examined. Reading, searching, watching, operating software, or applying an action are
evidence channels and do not qualify by themselves. The validator may reject every broad learning
activity; a successful empty `learning_subjects` array is the canonical no-learning result.
Otherwise it records atomic learning units, visible search queries, source material, and one
evidence-backed depth:
`lookup`, `orientation`, `focused-explanation`, `procedural`, `applied`, or `synthesis`.
Depth describes observable exposure and engagement, not comprehension or retention. Passive
activity is not considered unattended merely because keyboard and mouse input stopped. `unattended`
and agentic-coding environment metadata remain exclusively on broad activity subjects.

The unified request uses one chronological evidence selection and one payload count. Capture
continues while input-idle, but stops while Windows is locked and honors the foreground denylist.
Model description runs separately from capture so a slow request does not create a gap in the
following block. The chronological `journal.md` is rebuilt from report fields in JSON block
artifacts; structured activity and learning subjects remain canonical in JSON.

Block schema version 3 stores the activity description, attention judgments, agentic-coding
environment, validated learning units, and unified request frame count in one strict artifact.
Separate learning artifacts are no longer written. Older block or pending-manifest shapes are not
accepted. Recent blocks remain available in full. Older blocks are projected into a structured
aggregate and folded once into the bounded plaintext `summary.md`, including their validated
learning and agentic-coding context.

The generated portion of a described block has this shape:

```json
{
  "title": "Software development: revised activity generation",
  "report": "...",
  "subjects": [
    {
      "namespaces": ["software-development", "agentic-coding", "ashe-worker"],
      "subject": "Used Codex to revise the activity pipeline.",
      "estimated_duration_s": 420,
      "unattended": false,
      "agentic_coding": {
        "medium": "tui",
        "tool": "codex",
        "model_id": "gpt-5.4"
      }
    }
  ],
  "learning_subjects": [
    {
      "namespaces": ["learning", "applied", "rust", "serde-schema"],
      "subject": "Applied strict Serde fields to the unified artifact schema.",
      "estimated_duration_s": 120,
      "learning": {
        "search_queries": [],
        "sources": [{ "kind": "code", "title": "block_artifact.rs" }],
        "depth": "applied"
      }
    }
  ]
}
```

`agentic_coding` is required exactly on `software-development/agentic-coding` activity subjects;
`model_id` is optional and omitted when it is not visible. `learning_subjects` is always present and
may be empty after a successful validation.

At the end of each day, the worker generates `daily.md` deterministically without a daily model
request. It projects block titles, activity subjects, validated learning units, subject estimates,
and measured timelines; identifies missing and pending coverage gaps; totals active, idle,
application, and outcome telemetry; and aggregates estimated duration and attention counts for
every activity namespace prefix. Namespace durations remain independent estimates and may overlap.

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

The supplied CLI centralizes manual archive operations. To upload an existing encrypted archive
without decrypting or re-encrypting it:

```powershell
.\ashe-worker-cli.exe archive upload <archive>
```

To archive a closed day immediately, rather than waiting for the retention scan, provide its
calendar date. The command refuses current, future, and pending days; generates and validates the
deterministic daily report when enabled; verifies the encrypted archive; and only then removes the
plaintext and image artifacts:

```powershell
.\ashe-worker-cli.exe archive seal 2026-07-27
```

To restore an archive into a new output directory, run the decrypt command and enter the
passphrase at its hidden prompt:

```powershell
.\ashe-worker-cli.exe archive decrypt <archive> <output-directory>
```

Commands that inspect local artifacts use the worker's configured artifact directory. Pass
`--artifacts-dir <path>` before `archive` to override it for `seal`.

Remote backup is an authenticated HTTPS upload to a write-only receiver. The worker records
successful acknowledgements locally to avoid sending the same encrypted archive on every scan;
the receiver exposes no archive inventory or download capability.

## Build and release

Build the Windows GUI and CLI binaries from the project root:

```powershell
cargo build --release --target x86_64-pc-windows-gnu
```

From Linux or WSL, the shipping script cross-builds the worker and unified CLI, then
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
