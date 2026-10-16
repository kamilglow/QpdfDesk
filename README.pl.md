# QpdfDesk

*[English version](README.md)*

Mały program dla Windows, dzięki któremu z [QPDF](https://qpdf.readthedocs.io/) korzysta się bez wpisywania komend: **przeciągnij pliki PDF, zaznacz kroki, naciśnij Start.** Scalanie, dzielenie, wybór i obracanie stron, zabezpieczanie hasłem i jego zdejmowanie, optymalizacja, naprawa, usuwanie metadanych, spłaszczanie formularzy, dodawanie i wyciąganie załączników, nakładanie jednego PDF-a na drugi — każdy wynik sprawdzony przez QPDF, zanim dostanie swoją nazwę, a oryginał nigdy nie jest ruszany.

![QpdfDesk po partii siedmiu plików: pięć zoptymalizowanych, jeden naprawiony z ostrzeżeniami, jeden czeka na hasło](docs/screenshot.png)

## Pobieranie

Pobierz `QpdfDesk-1.0.0-win64.zip` ze [strony wydań](../../releases/latest), sprawdź jego SHA-256 z opublikowanym tam, rozpakuj gdziekolwiek i postępuj według `READ_ME_FIRST.txt`: QPDF nie ma w paczce — pobierasz go raz z [oficjalnych wydań](https://github.com/qpdf/qpdf/releases) i rozpakowujesz obok `QpdfDesk.exe`. Nic się nie instaluje.

## Możliwości

- **Jeden łańcuch, jedno przejście** — wybrane kroki to jedno wywołanie QPDF na wynik, bez plików pośrednich.
- **Strony** — zostaw, usuń albo odwróć (`1-5,8`, `z` = ostatnia; resztę składni pokazuje dymek pola) i obróć; każdy plik osobno albo wszystkie scalone w jeden lub przeplecione strona po stronie. Wybrane strony dotyczą każdego pliku; *Arrange pages…* daje jednemu plikowi własne — usunięte i ułożone na nowo, po numerze lub etykiecie. Scalanie nie zachowuje zakładek (spisu treści): QPDF buduje wynik od pustego dokumentu.
- **Pieczątka / tło** (*Stamp / background*) — strony innego PDF-a na wierzchu Twoich stron (pieczątka, znak wodny) albo pod nimi (papier firmowy): jego strona 1 na każdej stronie, jego strony po kolei albo na zmianę (jedno tło dla stron nieparzystych, drugie dla parzystych) — na wszystkich stronach albo na wpisanych.
- **Formularze i adnotacje** — spłaszcz je w stronę albo usuń formularz, zachowując wygląd pól.
- **Załączniki** — osadź pliki, usuń istniejące albo bezpiecznie je wyciągnij: wrogie nazwy są rozbrajane, a każdy wyciągnięty plik dostaje znacznik „pobrany z internetu”.
- **Prywatność** — usuń informacje o dokumencie i metadane XMP, stały identyfikator dokumentu; w Advanced drzewo struktury i etykiety stron.
- **Optymalizacja** — bezstratna (obrazy nietknięte) albo mniejsza: obrazy zapisane jako JPEG, a JPEG-i, które plik już ma, zostają bez zmian — chyba że ustawisz jakość, wtedy są kompresowane ponownie; „zostaw oryginał, jeśli wynik nie jest mniejszy”. JPEG zapisuje kolor w połowie rozdzielczości (4:2:0) i QPDF nie pozwala tego zmienić: dla wiernych kolorów wybierz tryb bezstratny.
- **Ochrona** — zablokuj hasłem (AES-256, hasło otwarcia i właściciela, typowe uprawnienia) albo odblokuj: bez hasła i bez ograniczeń.
- **Wynik** (*Output*) — zawsze włączony: gdzie trafia i jak się nazywa, podział na pliki po N stron (części nie mają zakładek), szybki podgląd w przeglądarce.
- **Wyłączony krok** ma wyszarzone opcje: nic nie robią, dopóki go nie włączysz.
- **Check i Info** — `qpdf --check` dowolnego pliku z raportem; co plik zawiera (strony, szyfrowanie, etykiety, załączniki, metadane).
- **Profile** — zapisz zestaw kroków i opcji pod nazwą: tylko zaznaczone kroki i ich opcje, więc profil nie trzyma ścieżek plików kroku, którego nie używa. Każde uruchomienie zaczyna się bez zaznaczonego kroku: wtedy przycisk Start nazywa się Repair (naprawa).
- **Command…** — dokładna komenda `qpdf` dla zaznaczonego pliku, z hasłami zamaskowanymi, gotowa do wklejenia w cmd.exe: tworzy identyczny plik.
- **Bezpieczny z założenia** — oryginał nigdy nie jest nadpisywany; wynik trafia do pliku tymczasowego, jest sprawdzany przez `qpdf --check` (z liczbą stron) i dopiero wtedy dostaje nazwę, nigdy na miejsce istniejącego pliku. Hasła żyją tylko w pamięci: nigdy w wierszu poleceń, w pliku, w raporcie ani w schowku.
- **Lekki** — jeden plik EXE ~1,4 MB, bez instalatora, około 4 MB pamięci i zero procesora w spoczynku; QPDF działa z niskim priorytetem w zadaniu Windows, które ogranicza jego pamięć i kończy go razem z programem.

## Wymagania

- Windows 10 lub 11, 64-bit.
- QPDF **12.4 lub nowszy** (oficjalny ZIP `msvc64`). QpdfDesk szuka go najpierw obok siebie (`qpdf\bin`, `qpdf-*\bin`, `bin`), potem w Program Files, potem w `PATH`. Bez niego okno i tak się otwiera i mówi, czego brakuje.

## Ustawienia

`settings.txt` obok programu zapisuje się przy zamknięciu: rozmiar okna i ostatni folder. Trzy opcje są dostępne tylko tam:

```ini
threads=
qpdf_memory_mb=
language=
```

Puste znaczy automatycznie. `threads` — ile plików naraz jest przetwarzanych (i sprawdzanych pod kątem stron), 1–16; automatycznie: połowa logicznych rdzeni procesora, najwyżej 4. QPDF używa jednego rdzenia na plik, więc dwa naraz mniej więcej skracają długą partię o połowę (zmierzone: 32 pliki 26,6 s → 12,7 s), a QPDF zawsze działa z niskim priorytetem. `qpdf_memory_mb` — pamięć, jaką mogą zająć wszystkie procesy QPDF razem (automatycznie: połowa RAM, najwyżej 8 GB). `language` — język okna (`en`); automatycznie: język Windows, gdy QpdfDesk go ma, inaczej angielski. Wartości spoza zakresu znaczą automatycznie.

Pliki i foldery upuszcza się na listę, która podczas przeciągania dostaje obramowanie; tam, gdzie nic upuszczenia nie przyjmuje, kursor pokazuje zakaz. PDF upuszczony na pole pliku w *Stamp / background* staje się pieczątką, pliki upuszczone na opcje *Attachments* zostają dołączone, a folder upuszczony na pole folderu w *Output* przyjmuje wyniki — jeden plik albo jeden folder tam, gdzie pasuje tylko jeden.

Dodawanie folderu można przerwać przyciskiem Cancel (w miejscu Start) albo klawiszem Esc — przydatne, gdy przez pomyłkę upuszczono cały dysk. Okno „About” jest w menu paska tytułu (prawy przycisk na nim) i pod F1.

Lista mieści do 10 000 plików naraz; większy folder dodaje pierwsze 10 000 w kolejności nazw i mówi, ile pominął.

## Budowanie ze źródeł

```bash
cargo run              # build deweloperski
cargo build --release  # zoptymalizowany pod rozmiar
cargo test             # testy; integracyjne używają QPDF z bin\
```

Funkcja `fltk-bundled` pobiera gotowy FLTK, więc CMake nie jest potrzebny. Biblioteka C jest linkowana statycznie (`.cargo/config.toml`), więc EXE nie wymaga pakietu Visual C++ Redistributable. Ikona i blok wersji powstają w `build.rs`. Testy integracyjne oczekują oficjalnych ZIP-ów QPDF rozpakowanych w `bin\qpdf-12.4.2-msvc64` i `bin\qpdf-12.4.2-mingw64`; bez nich są pomijane.

## Tłumaczenie

Każdy napis okna jest w `lang/en.txt` (`klucz = tekst`), wkompilowanym w program. Język to `lang/<kod>.txt` z tymi samymi kluczami — klucz, którego brakuje, pokazuje się po angielsku — i jedna linia w `LANGUAGES` w `src/text.rs`. Liczba mnoga ma formy `one`, `few` i `many`. Katalog, który wyświetlałby się źle, nie zbuduje się: nieznany lub powtórzony klucz, inne wartości wstawiane do tekstu niż po angielsku, znak, którego przycisk, menu albo lista rozwijana nie pokaże — błąd wskazuje plik i linię. Build deweloperski uruchomiony z `QD_LANG=<kod>` pokazuje dany język, a z `QD_FIT=<plik>` wypisuje każdą etykietę, która nie mieści się w swoim miejscu.

Zmiany w kolejnych wydaniach opisuje [CHANGELOG.md](CHANGELOG.md) (po angielsku).

## Licencja

QpdfDesk jest wydany na licencji MIT — zob. [LICENSE](LICENSE).

QPDF to osobny projekt Jaya Berkenbilta i współtwórców na licencji Apache 2.0. QpdfDesk jedynie **uruchamia `qpdf.exe` jako osobny proces** i nie zawiera go ani nie linkuje.
