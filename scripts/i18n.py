#!/usr/bin/env python3
"""Translation tooling for Alcedo (standard library only).

  i18n.py extract            scan the sources for tr!/trs!/trc!/trf!/ntr!/n_! and write po/<domain>.pot
  i18n.py update             merge the .pot into every po/*.po (adds new, drops obsolete)
  i18n.py compile [OUT_DIR]  build OUT_DIR/<lang>/LC_MESSAGES/<domain>.mo (default target/locale)
  i18n.py check              fail on untranslated, fuzzy or stale entries, mismatched
                             placeholders, wrong plural counts, or strftime patterns GLib
                             cannot format

Run directly, it works on this repository (domain `alcedo`, every crate's src/).
Another repository that uses the alcedo-i18n crate can run the same rules on its
own text without a copy of this file: load it with importlib and call
`main(sys.argv, Project.for_repository(...))` with its own root, domain and sources.

xgettext and msgfmt are not required, so this works in the Flatpak sandbox
and on a bare development machine. Extraction only sees plain string
literals as the first argument(s) of the macros in crates/alcedo-i18n; comments may
sit between the parenthesis and the literal. It cannot find strings that were
never wrapped, so review new user-visible text by eye.
"""

import re
import struct
import sys
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Project:
    """One repository's translations: where its sources and catalogues live,
    its gettext domain, and how catalogue headers name it."""

    root: Path
    domain: str
    sources: tuple
    po_dir: Path
    out_dir: Path
    name: str
    licence: str

    @classmethod
    def for_repository(cls, root, domain, sources, name, licence):
        """The usual layout: catalogues in `root/po`, compiled into `root/target/locale`."""
        root = Path(root)
        return cls(root, domain, tuple(sources), root / "po", root / "target" / "locale", name, licence)

    @property
    def pot(self):
        return self.po_dir / f"{self.domain}.pot"

    def shown(self, path):
        """`path` relative to the root for messages, or in full when outside it."""
        return path.relative_to(self.root) if path.is_relative_to(self.root) else path


def _alcedo_project():
    root = Path(__file__).resolve().parent.parent
    # Every crate's sources except the macros' own crate (its docs and tests).
    sources = [src for src in sorted((root / "crates").glob("*/src")) if src.parent.name != "alcedo-i18n"]
    return Project.for_repository(root, "alcedo", sources, "Alcedo", "GPL-3.0-or-later")


ALCEDO = _alcedo_project()

PLURAL_FORMS = {
    "fr": "nplurals=2; plural=(n > 1);",
    "es": "nplurals=2; plural=(n != 1);",
    "de": "nplurals=2; plural=(n != 1);",
    "it": "nplurals=2; plural=(n != 1);",
}
LANGUAGE_NAMES = {"fr": "French", "es": "Spanish", "de": "German", "it": "Italian"}

LIT = r'"((?:[^"\\]|\\.)*)"'
# Whitespace and comments allowed between the macro's tokens.
GAP = r"(?:\s|//[^\n]*|/\*.*?\*/)*"
MACRO = re.compile(
    r"(?<![\w!])(tr|trs|trc|trf|ntr|n_)!\((" + GAP + r")" + LIT
    + r"(?:" + GAP + r"," + GAP + LIT + r")?",
    re.S,
)
PLACEHOLDER = re.compile(r"\{[a-z_][a-z0-9_]*\}")
STRFTIME_CONTEXT = "strftime pattern"
# Conversions g_date_time_format accepts (GLib formats every pattern first).
STRFTIME_CONVERSIONS = set("aAbBcCdeFgGhHIjklmMnpPrRsStTuVwxXyYzZ%")
STRFTIME_DIRECTIVE = re.compile(r"%([-_0]?)([EO]?)(.?)", re.S)


class Entry:
    def __init__(self, msgid, plural=None, context=None):
        self.msgid = msgid
        self.plural = plural
        self.context = context
        self.refs = []
        self.comments = []
        self.msgstr = ""  # singular form, or "" if untranslated
        self.msgstr_plural = []  # list of forms for plural entries
        self.fuzzy = False

    @property
    def key(self):
        return (self.context, self.msgid, self.plural)


def unescape(text):
    return bytes(text, "utf-8").decode("unicode_escape").encode("latin-1").decode("utf-8")


def escape(text):
    return (text.replace("\\", "\\\\").replace('"', '\\"')
            .replace("\n", "\\n").replace("\t", "\\t"))


def extract(sources, root):
    entries = {}
    paths = sorted(path for src in sources for path in src.rglob("*.rs"))
    for path in paths:
        text = path.read_text(encoding="utf-8")
        try:
            found = list(find_messages(text))
        except ValueError as err:
            raise SystemExit(f"{path.relative_to(root)}: {err}")
        for entry, line_no, comments in found:
            existing = entries.setdefault(entry.key, entry)
            existing.refs.append(f"{path.relative_to(root)}:{line_no}")
            for comment in comments:
                if comment not in existing.comments:
                    existing.comments.append(comment)
    return list(entries.values())


def find_messages(text):
    """Yield (Entry, line number, translator comments) for each macro call in `text`."""
    lines = text.split("\n")
    for match in MACRO.finditer(text):
        kind, gap, first, second = match.groups()
        line_no = text.count("\n", 0, match.start()) + 1
        if any("\\\n" in lit for lit in (first, second) if lit):
            # Rust drops the newline and the next line's indentation; this tool
            # cannot, so the msgid would never match at run time.
            raise ValueError(f"line {line_no}: a translated literal uses a \\ line continuation; write it on one line")
        if kind == "trc":
            if second is None:
                continue
            entry = Entry(unescape(second), context=unescape(first))
        elif kind == "ntr":
            if second is None:
                continue
            entry = Entry(unescape(first), plural=unescape(second))
        else:
            entry = Entry(unescape(first))
        comments = translator_comments(lines, line_no - 1) + translator_comments(gap.split("\n"), None)
        yield entry, line_no, comments


def translator_comments(lines, index):
    """The `// TRANSLATORS:` block directly above line `index`, or the one ending
    `lines` when `index` is None (comments inside the macro call)."""
    found = []
    i = len(lines) - 1 if index is None else index - 1
    while i >= 0 and (lines[i].strip().startswith("//") or (index is None and not lines[i].strip())):
        if lines[i].strip():
            found.insert(0, lines[i].strip().lstrip("/").strip())
        i -= 1
    if found and found[0].upper().startswith("TRANSLATORS:"):
        return found
    return []


def header(lang, project):
    forms = PLURAL_FORMS.get(lang, "nplurals=2; plural=(n != 1);")
    name = LANGUAGE_NAMES.get(lang, lang)
    title = "SOME DESCRIPTIVE TITLE." if lang is None else f"{name} translation of {project.name}."
    language = "" if lang is None else lang
    return (
        f"# {title}\n"
        f"# Copyright (C) Windward Logic. Same licence as {project.name} ({project.licence}).\n"
        "#\n"
        'msgid ""\n'
        'msgstr ""\n'
        f'"Project-Id-Version: {project.domain}\\n"\n'
        f'"Language: {language}\\n"\n'
        '"MIME-Version: 1.0\\n"\n'
        '"Content-Type: text/plain; charset=UTF-8\\n"\n'
        '"Content-Transfer-Encoding: 8bit\\n"\n'
        f'"Plural-Forms: {forms}\\n"\n\n'
    )


def render_entry(entry):
    out = []
    for comment in entry.comments:
        out.append(f"#. {comment}")
    for ref in entry.refs:
        out.append(f"#: {ref}")
    if entry.fuzzy:
        out.append("#, fuzzy")
    if entry.context is not None:
        out.append(f'msgctxt "{escape(entry.context)}"')
    out.append(f'msgid "{escape(entry.msgid)}"')
    if entry.plural is not None:
        out.append(f'msgid_plural "{escape(entry.plural)}"')
        forms = entry.msgstr_plural or ["", ""]
        for number, form in enumerate(forms):
            out.append(f'msgstr[{number}] "{escape(form)}"')
    else:
        out.append(f'msgstr "{escape(entry.msgstr)}"')
    return "\n".join(out) + "\n"


def write_po(path, lang, entries, project):
    path.parent.mkdir(parents=True, exist_ok=True)
    body = "\n".join(render_entry(e) for e in sorted(entries, key=lambda e: e.refs[0] if e.refs else ""))
    path.write_text(header(lang, project) + body, encoding="utf-8")


def parse_po(path):
    """Parse a .po file into Entry objects (header entry excluded)."""
    entries, current, field = [], None, None
    pending_comments, pending_refs, pending_fuzzy = [], [], False

    def finish():
        nonlocal current
        if current is not None and current.get("msgid") is not None:
            entries.append(current)
        current = None

    def start():
        nonlocal current
        if current is None:
            current = {"msgid": None, "ctx": None, "plural": None, "str": {}, "fuzzy": pending_fuzzy,
                       "comments": list(pending_comments), "refs": list(pending_refs)}

    for raw in path.read_text(encoding="utf-8").split("\n"):
        line = raw.strip()
        if not line:
            finish()
            pending_comments, pending_refs, pending_fuzzy, field = [], [], False, None
            continue
        if line.startswith("#."):
            if current is not None and current["msgid"] is not None:
                finish()
                pending_comments, pending_refs, pending_fuzzy = [], [], False
            pending_comments.append(line[2:].strip())
            continue
        if line.startswith("#:"):
            if current is not None and current["msgid"] is not None:
                finish()
                pending_comments, pending_refs, pending_fuzzy = [], [], False
            pending_refs.extend(line[2:].split())
            continue
        if line.startswith("#,"):
            pending_fuzzy = "fuzzy" in line
            continue
        if line.startswith("#"):
            continue
        m = re.match(r'^(msgctxt|msgid_plural|msgid|msgstr(?:\[(\d+)\])?)\s+' + LIT + "$", line)
        if m:
            start()
            kind, index, value = m.group(1), m.group(2), unescape(m.group(3))
            if kind == "msgctxt":
                current["ctx"], field = value, ("ctx",)
            elif kind == "msgid":
                current["msgid"], field = value, ("msgid",)
            elif kind == "msgid_plural":
                current["plural"], field = value, ("plural",)
            else:
                idx = int(index) if index is not None else 0
                current["str"][idx], field = value, ("str", idx)
            continue
        m = re.match("^" + LIT + "$", line)
        if m and current is not None and field is not None:
            value = unescape(m.group(1))
            if field[0] == "str":
                current["str"][field[1]] += value
            else:
                key = {"ctx": "ctx", "msgid": "msgid", "plural": "plural"}[field[0]]
                current[key] += value
            continue
        raise SystemExit(f"{path}: cannot parse line: {raw}")
    finish()

    result = []
    for item in entries:
        if item["msgid"] == "":
            continue  # header
        entry = Entry(item["msgid"], item["plural"], item["ctx"])
        entry.refs, entry.comments, entry.fuzzy = item["refs"], item["comments"], item["fuzzy"]
        if item["plural"] is not None:
            count = max(item["str"]) + 1 if item["str"] else 2
            entry.msgstr_plural = [item["str"].get(i, "") for i in range(count)]
        else:
            entry.msgstr = item["str"].get(0, "")
        result.append(entry)
    return result


def cmd_extract(project):
    entries = extract(project.sources, project.root)
    write_po(project.pot, None, entries, project)
    print(f"wrote {project.shown(project.pot)}: {len(entries)} strings")


def cmd_update(project):
    template = extract(project.sources, project.root)
    write_po(project.pot, None, template, project)
    for po in sorted(project.po_dir.glob("*.po")):
        lang = po.stem
        old = {e.key: e for e in parse_po(po)}
        merged, added = [], 0
        for entry in template:
            previous = old.get(entry.key)
            if previous is None:
                added += 1
            else:
                entry.msgstr, entry.msgstr_plural, entry.fuzzy = (
                    previous.msgstr, previous.msgstr_plural, previous.fuzzy)
            merged.append(entry)
        write_po(po, lang, merged, project)
        dropped = len(set(old) - {e.key for e in template})
        print(f"{po.name}: {len(merged)} strings, {added} new, {dropped} obsolete dropped")


def placeholders(text):
    return sorted(PLACEHOLDER.findall(text))


def problems(entries, lang):
    found = []
    expected_forms = 2
    for entry in entries:
        label = f"{lang}: {entry.msgid!r}"
        forms = entry.msgstr_plural if entry.plural is not None else [entry.msgstr]
        if not any(forms):
            found.append(("missing", f"{label}: untranslated"))
            continue
        if entry.fuzzy:
            found.append(("fuzzy", f"{label}: marked fuzzy"))
        if entry.plural is not None and len(forms) != expected_forms:
            found.append(("error", f"{label}: expected {expected_forms} plural forms, got {len(forms)}"))
        want = placeholders(entry.msgid)
        # A plural form may use any name from either English form ({n} is often
        # only in the plural), and must keep every name except the count.
        allowed = set(want) | set(placeholders(entry.plural or ""))
        required = allowed - {"{n}"}
        for form in forms:
            have = set(placeholders(form))
            if not form:
                found.append(("missing", f"{label}: empty plural form"))
            elif entry.plural is None and placeholders(form) != want:
                found.append(("error", f"{label}: placeholders {placeholders(form)} differ from {want}"))
            elif entry.plural is not None and (not have <= allowed or not required <= have):
                found.append(("error", f"{label}: placeholders {sorted(have)} do not fit {sorted(allowed)}"))
            elif (entry.context or "").startswith(STRFTIME_CONTEXT):
                for problem in strftime_problems(form):
                    found.append(("error", f"{label}: {problem}"))
    return found


def strftime_problems(pattern):
    """Directives in a translated date pattern that GLib cannot format."""
    found = []
    for match in STRFTIME_DIRECTIVE.finditer(pattern):
        conversion = match.group(3)
        if conversion == "":
            found.append(f"pattern {pattern!r} ends with a lone %")
        elif conversion not in STRFTIME_CONVERSIONS:
            found.append(f"pattern {pattern!r} uses unsupported %{match.group(1)}{match.group(2)}{conversion}")
    return found


def cmd_check(project):
    template = extract(project.sources, project.root)
    keys = {e.key for e in template}
    failed = False
    pot = project.pot
    pot_entries = {e.key: e for e in parse_po(pot)} if pot.exists() else {}
    if set(pot_entries) != keys:
        print(f"{project.shown(pot)} is out of date: run scripts/i18n.py update")
        failed = True
    for po in sorted(project.po_dir.glob("*.po")):
        entries = parse_po(po)
        have = {e.key for e in entries}
        for missing in sorted(keys - have, key=str):
            print(f"{po.stem}: not in catalogue (run update): {missing[1]!r}")
            failed = True
        issues = problems([e for e in entries if e.key in keys], po.stem)
        for kind, message in issues:
            print(message)
            failed = failed or kind == "error" or kind == "missing" or kind == "fuzzy"
        print(f"{po.name}: {len(entries)} entries, {sum(1 for k, _ in issues if k == 'missing')} untranslated")
    if failed:
        raise SystemExit(1)
    print("OK")


def build_mo(entries, lang, domain):
    messages = {}
    for entry in entries:
        forms = entry.msgstr_plural if entry.plural is not None else [entry.msgstr]
        if entry.fuzzy or not all(forms):
            continue  # fall back to English for anything incomplete
        key = entry.msgid if entry.context is None else f"{entry.context}\x04{entry.msgid}"
        if entry.plural is not None:
            key += "\x00" + entry.plural
        messages[key] = "\x00".join(forms)
    forms_line = PLURAL_FORMS.get(lang, "nplurals=2; plural=(n != 1);")
    messages[""] = (
        f"Project-Id-Version: {domain}\nContent-Type: text/plain; charset=UTF-8\n"
        f"Content-Transfer-Encoding: 8bit\nLanguage: {lang}\nPlural-Forms: {forms_line}\n")
    keys = sorted(messages, key=lambda k: k.encode("utf-8"))
    ids, strs, offsets = b"", b"", []
    for key in keys:
        kb, vb = key.encode("utf-8"), messages[key].encode("utf-8")
        offsets.append((len(ids), len(kb), len(strs), len(vb)))
        ids += kb + b"\x00"
        strs += vb + b"\x00"
    count = len(keys)
    table_start = 7 * 4
    id_table = table_start
    str_table = id_table + count * 8
    data_start = str_table + count * 8
    id_data = data_start
    str_data = id_data + len(ids)
    out = struct.pack("<7I", 0x950412DE, 0, count, id_table, str_table, 0, 0)
    out += b"".join(struct.pack("<2I", length, id_data + off) for off, length, _, _ in offsets)
    out += b"".join(struct.pack("<2I", length, str_data + off) for _, _, off, length in offsets)
    return out + ids + strs


def cmd_compile(project, out_dir):
    out_dir = Path(out_dir)
    for po in sorted(project.po_dir.glob("*.po")):
        target = out_dir / po.stem / "LC_MESSAGES" / f"{project.domain}.mo"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(build_mo(parse_po(po), po.stem, project.domain))
        print(f"wrote {target}")


def main(argv, project=ALCEDO):
    command = argv[1] if len(argv) > 1 else ""
    if command == "extract":
        cmd_extract(project)
    elif command == "update":
        cmd_update(project)
    elif command == "compile":
        cmd_compile(project, argv[2] if len(argv) > 2 else project.out_dir)
    elif command == "check":
        cmd_check(project)
    else:
        print(__doc__)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
