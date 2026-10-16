# Changelog

All notable changes to QpdfDesk. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [Semantic Versioning](https://semver.org/).

## [1.0.0] - 2026-10-16

The first public release.

### Added
- **A list of PDF files:** drag files or whole folders onto it (up to 10,000 files); the list is outlined while a drag is over it. A file field takes a dropped path too: the PDF of Stamp / background, the files to attach, the folder of the results — one where only one fits. The list keeps the order files were added in — it is the order of a merge — and can be reordered, sorted by name, or cleared. A header names its columns; a name takes the room it needs, and one too long for it is cut in the middle, so its end stays in view. Adding a large folder can be stopped with Cancel or Esc.
- **Steps, run as one QPDF call per result:**
  - **Pages** — keep, delete or reverse pages and rotate them; each file on its own, or every file merged into one or mixed page by page (a scan of the odd pages and one of the even pages). *Arrange pages…* gives a file pages of its own — removed and reordered, by number or page label — in place of the ones chosen for all.
  - **Stamp / background** — the pages of another PDF on top of the pages or under them: its page 1 on each page, its pages in order, or its pages in turn (one background for odd pages, another for even ones) — on every page or on the pages typed.
  - **Forms & annotations** — flatten them into the page, or remove the form and keep the fields' look.
  - **Attachments** — embed files, or remove the ones a file has.
  - **Privacy** — remove the document info and XMP metadata, a document ID made from the content; in Advanced, the structure tree and page labels.
  - **Optimization** — lossless (images untouched), or smaller with images re-encoded as JPEG: the JPEGs a file has stay as they are, unless a quality is set, which compresses them again; "Keep original if not smaller".
  - **Protection** — lock with a password (AES-256, with the usual permissions), or unlock: no password, no restrictions.
  - **Output** — always on: the folder and name of the results, split into files of N pages, fast web view.
  - Every start begins with no step chosen; then the Start button says Repair and repairs each file. The options of a step switched off are grey.
- **Results are safe:** each is written next to its original (or in a chosen folder) as `name (step).pdf`, checked with `qpdf --check` and its page count before it gets its name, and never written over an existing file. The original is never changed.
- **Check** (QPDF's own check of the selected or of every file, with a report to save), **Info** (what a file holds: pages, PDF version, encryption, forms, bookmarks, page labels, metadata, attachments), and pulling attachments out of a file.
- **Profiles:** named sets of the steps chosen and their options — nothing of a step left off; `*` after the profile's name shows the panel was changed in a way that changes the result.
- **Command…** shows the `qpdf` command that makes the selected file's result, ready for cmd.exe, with every password masked.
- Several files at once (half the processor's logical cores, at most 4), and a background look at every added file: page count, password needed.
- Light and dark theme, following Windows. About in the title bar's menu and on F1. The window's icon is drawn at the size Windows asks for, so it stays sharp on the taskbar at any display scale.
- **Ready for translations:** every text the window shows comes from a catalog compiled into the program (`lang/en.txt`; English for now). The window speaks the language of Windows when it has it, else English; `language=` in `settings.txt` chooses one. Profiles keep each drop-down by a fixed name, so a new wording or language keeps what was saved.

### Security
- QPDF is started by its full path only. Every argument, passwords included, goes to it through stdin, never on the command line.
- Passwords stay in memory only, are wiped when no longer needed, and are never saved, shown, copied or reported.
- DLLs are loaded from System32 only.
- Every QPDF process runs below normal priority inside one Windows job: a combined memory limit, a watchdog for a process that hangs, and nothing left running when the app closes. QPDF's own safety limits are always on.
- Attachment names from a PDF are made safe before anything is written, and every extracted file gets the "downloaded from the internet" mark.
- Text from PDFs and file names is shown without control or direction-changing characters.
- No network access, no telemetry, no log files.

### Known limitations
- A merge keeps no bookmarks (QPDF builds it from an empty document); page labels and form fields are kept. Nor do the parts of a split file.
- QPDF cannot open a path longer than 259 characters.
- Optimization › Smaller re-encodes images but never lowers their resolution. JPEG keeps the colour at half the resolution (4:2:0), which QPDF offers no way to change: for exact colours, use Lossless.
- On FAT or exFAT drives extracted attachments cannot get the "downloaded from the internet" mark.

### Measured
- Program file: 1,402 KB; the ZIP 665 KB. Memory: 4.3 MB idle (private working set). Processor use while idle: none (0 ms in 20 s). Start to window: 79–91 ms (361 ms the first time a new build runs).
- 10,000 files on the list: no pause of the window longer than 31 ms while they are added (long names, cut to their column), 27 ms when a batch of them starts, 30 ms when they are sorted by name.
- QPDF itself: 13–36 MB of memory at its peak on the test files (a 100 MB file included); 32 copies of the QPDF manual took 26.6 s one at a time, 12.7 s two at a time, 8.8 s four at a time.
