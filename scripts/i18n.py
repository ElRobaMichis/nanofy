#!/usr/bin/env python3
"""Mantiene las traducciones de Nanofy (lang/<código>.csv).

    python scripts/i18n.py            # añade a cada CSV los textos nuevos del código (vacíos)
    python scripts/i18n.py --check    # dice cuántos faltan en cada idioma (sin tocar nada)
    python scripts/i18n.py --new fr Français   # crea lang/fr.csv con todos los textos vacíos

Los textos son los que el código marca con t("…"), t_owned("…"), tr!("…") o tk!("…") (en
constantes): el español es la clave. Las filas que empiezan por @ no son textos: @name (nombre del
idioma), @thousands y @decimal (separadores de miles y de decimales; vacíos, los del español). Las filas que ya no están en el código se quitan (y se listan). El orden es el de la
primera aparición en el código, para que textos de una misma pantalla queden juntos.
"""
import csv
import io
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LANG = ROOT / "lang"
# t("…") / t_owned("…") / tr!("…" con o sin argumentos). Cadenas normales de Rust con escapes.
PATTERN = re.compile(r'\b(?:t|t_owned|tr!|tk!)\(\s*"((?:[^"\\]|\\.)*)"', re.DOTALL)
ESCAPES = {"n": "\n", "t": "\t", '"': '"', "\\": "\\", "'": "'", "0": "\0"}


def unescape(s: str) -> str:
    out, i = [], 0
    while i < len(s):
        c = s[i]
        if c == "\\" and i + 1 < len(s):
            n = s[i + 1]
            if n == "u" and s[i + 2:i + 3] == "{":
                j = s.index("}", i)
                out.append(chr(int(s[i + 3:j], 16)))
                i = j + 1
                continue
            if n == "\n":  # continuación de línea: se salta el salto y la sangría
                i += 2
                while i < len(s) and s[i] in " \t":
                    i += 1
                continue
            out.append(ESCAPES.get(n, n))
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def keys_in_code() -> list[str]:
    seen, keys = set(), []
    for path in sorted((ROOT / "src").rglob("*.rs")):
        text = path.read_text(encoding="utf-8")
        # Fuera las pruebas: lo de `#[cfg(test)]` en adelante no se ve en la app.
        cut = text.find("#[cfg(test)]")
        if cut >= 0:
            text = text[:cut]
        for m in PATTERN.finditer(text):
            k = unescape(m.group(1))
            if k and k not in seen:
                seen.add(k)
                keys.append(k)
    return keys


def read(path: Path) -> tuple[list[str], dict[str, str]]:
    rows = list(csv.reader(io.StringIO(path.read_text(encoding="utf-8-sig"))))
    header = rows[0] if rows else ["es", path.stem]
    return header, {r[0]: (r[1] if len(r) > 1 else "") for r in rows[1:] if r}


META = ("@name", "@thousands", "@decimal")


def write(path: Path, header: list[str], meta: dict[str, str], keys: list[str], tr: dict[str, str]) -> None:
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    w.writerow(header)
    for k in META:
        w.writerow([k, meta.get(k, "")])
    for k in keys:
        w.writerow([k, tr.get(k, "")])
    # Saltos LF también en Windows: un CR dentro de un texto de varias líneas cambiaría la clave.
    path.write_text(buf.getvalue(), encoding="utf-8", newline="\n")


def main() -> int:
    args = sys.argv[1:]
    keys = keys_in_code()
    LANG.mkdir(exist_ok=True)
    if args[:1] == ["--new"]:
        code, name = args[1], " ".join(args[2:]) or args[1]
        path = LANG / f"{code}.csv"
        if path.exists():
            print(f"{path} ya existe")
            return 1
        write(path, ["es", code], {"@name": name}, keys, {})
        print(f"creado {path} con {len(keys)} textos por traducir")
        return 0
    check = "--check" in args
    missing_total = 0
    for path in sorted(LANG.glob("*.csv")):
        header, tr = read(path)
        meta = {k: tr.pop(k) for k in list(tr) if k.startswith("@")}
        meta.setdefault("@name", path.stem)
        missing = [k for k in keys if not tr.get(k, "").strip()]
        gone = [k for k in tr if k not in set(keys)]
        missing_total += len(missing)
        print(f"{path.name}: {len(keys) - len(missing)}/{len(keys)} traducidos, {len(missing)} sin traducir, {len(gone)} que ya no se usan")
        if check:
            for k in missing[:20]:
                print("   falta:", k.replace("\n", "\\n"))
            if len(missing) > 20:
                print(f"   … y {len(missing) - 20} más")
        else:
            write(path, header, meta, keys, tr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
