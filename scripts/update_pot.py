#!/usr/bin/env python3
"""Extract tr("…") and ntr("…", "…") strings from tudi-gui into po/tudi.pot.

xgettext's C parser chokes on Rust lifetimes, so this does a simple regex
extraction (string literals without escapes other than \\" and \\n).
Then update catalogs with: msgmerge -U po/zh_CN.po po/tudi.pot
"""
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "crates/tudi-gui/src"
LIT = r'"((?:[^"\\]|\\.)*)"'
TR = re.compile(r"\btr\(\s*&?" + LIT + r"\s*\)")
NTR = re.compile(r"\bntr\(\s*" + LIT + r"\s*,\s*" + LIT)


def main():
    singles, plurals, refs = {}, {}, {}
    for path in sorted(SRC.glob("*.rs")):
        text = path.read_text()
        rel = path.relative_to(ROOT)
        for m in TR.finditer(text):
            line = text.count("\n", 0, m.start()) + 1
            singles.setdefault(m.group(1), None)
            refs.setdefault(m.group(1), []).append(f"{rel}:{line}")
        for m in NTR.finditer(text):
            line = text.count("\n", 0, m.start()) + 1
            plurals.setdefault(m.group(1), m.group(2))
            refs.setdefault(m.group(1), []).append(f"{rel}:{line}")
    out = [
        'msgid ""',
        'msgstr ""',
        '"Project-Id-Version: tudi\\n"',
        '"Content-Type: text/plain; charset=UTF-8\\n"',
        '"Content-Transfer-Encoding: 8bit\\n"',
        "",
    ]
    for msgid in singles:
        out += [f"#: {' '.join(refs[msgid])}", f'msgid "{msgid}"', 'msgstr ""', ""]
    for msgid, plural in plurals.items():
        out += [
            f"#: {' '.join(refs[msgid])}",
            f'msgid "{msgid}"',
            f'msgid_plural "{plural}"',
            'msgstr[0] ""',
            'msgstr[1] ""',
            "",
        ]
    (ROOT / "po/tudi.pot").write_text("\n".join(out))
    print(f"{len(singles)} strings, {len(plurals)} plurals")


if __name__ == "__main__":
    main()
