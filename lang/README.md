# Translations

Nanofy's interface is written in Spanish, and each other language is one CSV file in this folder:
`en.csv` (English), and any you add (`fr.csv`, `pt.csv`, `de.csv`…). The file name is the language
code (ISO 639-1).

Each file has two columns: the Spanish text and its translation.

```csv
es,en
@name,English
@thousands,","
@decimal,.
Inicio,Home
Canciones que te gustan,Liked Songs
{} canciones,{} songs
```

- `@name` is the language's name as shown in **Settings → Language** (write it in that language:
  `Français`, `Português`).
- `@thousands` and `@decimal` are the separators for numbers (`1,196,073` and `4.7` in English).
  Leave them empty to use the Spanish ones (`1.196.073`, `4,7`).
- `{}` is a value Nanofy fills in (a number, a name…). Keep the same number of `{}` in your
  translation. If your language needs them in another order, number them: `{1} de {0}` → `{0}'s {1}`.
- Leave a translation empty and that text shows in Spanish: a partial translation never breaks
  anything.
- Open the file in Excel, LibreOffice or Google Sheets (save it as CSV, UTF-8), or in any text
  editor. Texts with commas, quotes or line breaks go between double quotes, with inner quotes
  doubled: `"Dijo ""hola""","Said ""hi"""`.

## Add a language

```sh
python scripts/i18n.py --new fr Français
```

This creates `lang/fr.csv` with every text and an empty second column. Fill it in, then open a pull
request with just that file.

## Try it without compiling

Put your CSV in a `lang` folder next to `nanofy.exe` (or the `nanofy` binary), open Nanofy and pick
it in **Settings → Language**. A file there overrides the built-in one with the same name, so you
can also test fixes to `en.csv` this way.

## Keep a translation up to date

When Nanofy gets new texts, run:

```sh
python scripts/i18n.py          # adds the new texts (empty) to every CSV and drops unused ones
python scripts/i18n.py --check  # how many are missing in each language, without changing anything
```

## For developers

In the code, every visible text goes through the translation functions in `src/i18n.rs`, with the
Spanish text as the key:

```rust
t("Canciones que te gustan")          // &'static str
tr!("{} canciones", n)                // String with values
const FILTERS: [&str; 2] = [tk!("Todo"), tk!("Artistas")]; // in constants; use t(FILTERS[i])
```

Then run `python scripts/i18n.py` so the new texts reach the CSV files.

---

**En español:** cada idioma es un CSV con el texto en español y su traducción. Para añadir uno:
`python scripts/i18n.py --new fr Français`, rellena la segunda columna y abre un pull request con
ese archivo. Lo que quede vacío se ve en español.
