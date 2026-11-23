#!/usr/bin/env python3
"""Pone la versión de la etiqueta de git (v1.2.3) en tauri.conf.json y Cargo.toml."""
import json
import re
import sys
from pathlib import Path

tag = sys.argv[1].lstrip("v")
if not re.fullmatch(r"\d+\.\d+\.\d+", tag):
    sys.exit(f"la etiqueta '{sys.argv[1]}' no es una versión tipo v1.2.3")

root = Path(__file__).resolve().parent.parent

conf = root / "src-tauri/tauri.conf.json"
data = json.loads(conf.read_text())
data["version"] = tag
conf.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")

cargo = root / "src-tauri/Cargo.toml"
text = cargo.read_text()
text, n = re.subn(r'(?m)^version = "[^"]*"$', f'version = "{tag}"', text, count=1)
assert n == 1, "no se encontró la línea version en Cargo.toml"
cargo.write_text(text)

print(f"versión {tag} aplicada")
