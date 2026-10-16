QpdfDesk 1.0.0 - read me first
==============================

QpdfDesk lets you use QPDF without typing commands: drop PDF files on it, tick
the steps you want (merge, split, pick pages, flatten forms and annotations,
protect, optimize, repair, clean metadata, attachments, stamps) and press
Start. The originals are never changed; every result is a new file next to the
original, checked by QPDF before it gets its name.

QpdfDesk does NOT include QPDF. You need QPDF 12.4 or newer, once:

1. Open the official QPDF release page - and only that one:
       https://github.com/qpdf/qpdf/releases
   Take the newest 12.x release and download two files from it:
       qpdf-12.x.y-msvc64.zip      (Windows, 64-bit)
       qpdf-12.x.y.sha256          (the checksums)

2. Check the download. In PowerShell, in the download folder:
       Get-FileHash .\qpdf-12.x.y-msvc64.zip -Algorithm SHA256
   The hash shown must be the one written next to "qpdf-12.x.y-msvc64.zip"
   in qpdf-12.x.y.sha256 (letter case does not matter). If it differs, delete
   the file and do not use it.

3. Unpack the ZIP into the folder of QpdfDesk.exe, so that this file exists:
       <QpdfDesk folder>\qpdf-12.x.y-msvc64\bin\qpdf.exe
   Do not move qpdf.exe out of its "bin" folder: it needs the DLLs beside it.

Start QpdfDesk.exe. Its title shows the QPDF it found, e.g. "QPDF 12.4.2". It
also finds a QPDF installed with the official installer (Program Files) or one
on PATH; a copy next to QpdfDesk.exe comes first.

Windows may warn about an unknown publisher ("Windows protected your PC"): the
program is not signed. Choose "More info", then "Run anyway" - after checking
the SHA-256 of the QpdfDesk ZIP against the one published with the release.

What it keeps, and where: only in its own folder - settings.txt (window size,
last folder), profiles.txt (saved profiles) and app.lock (while it runs). Passwords are never saved. Nothing goes to the
registry or the network. To remove QpdfDesk, delete its folder.

Hidden settings (settings.txt, edit while QpdfDesk is closed; empty means
automatic):
    threads=            files processed at once, 1-16
                        (automatic: half the logical cores, at most 4)
    qpdf_memory_mb=     memory limit for all QPDF processes together, in MB
                        (automatic: half the RAM, at most 8 GB)
    language=           language of the window, e.g. en
                        (automatic: that of Windows, else English)

The list holds up to 10,000 files at a time.

License: MIT (see LICENSE). QPDF is a separate program under its own license
(Apache 2.0), by Jay Berkenbilt and contributors.


QpdfDesk 1.0.0 - przeczytaj najpierw (po polsku)
================================================

QpdfDesk to prosta nakładka graficzna na QPDF, dzięki której nie trzeba
wpisywać komend: przeciągasz pliki PDF, zaznaczasz kroki i naciskasz Start.
Oryginały nigdy nie są zmieniane — wynik to nowy plik obok oryginału.

QPDF nie ma w paczce. Potrzebny jest QPDF 12.4 lub nowszy, jednorazowo:

1. Tylko z oficjalnej strony https://github.com/qpdf/qpdf/releases pobierz
   z najnowszego wydania 12.x pliki qpdf-12.x.y-msvc64.zip
   i qpdf-12.x.y.sha256.
2. W PowerShellu: Get-FileHash .\qpdf-12.x.y-msvc64.zip -Algorithm SHA256
   Wynik musi być taki sam jak w pliku .sha256 przy nazwie ZIP-a. Jeśli jest
   inny — usuń plik.
3. Rozpakuj ZIP do folderu QpdfDesk.exe, tak żeby istniał plik
   <folder QpdfDesk>\qpdf-12.x.y-msvc64\bin\qpdf.exe

Windows może ostrzec o nieznanym wydawcy (program nie jest podpisany):
„Więcej informacji” → „Uruchom mimo to” — po sprawdzeniu SHA-256 paczki
QpdfDesk z opublikowanym przy wydaniu.

Program zapisuje tylko we własnym folderze (settings.txt, profiles.txt,
app.lock); haseł nie zapisuje nigdy. Usunięcie = skasowanie folderu.
Lista mieści do 10 000 plików naraz.
