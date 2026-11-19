"""Behaviour tests for scripts/i18n.py.

Usage: python3 -m unittest discover -s scripts/tests
"""

import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "i18n.py"
SPEC = importlib.util.spec_from_file_location("i18n", SCRIPT)
i18n = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(i18n)


def messages(text):
    return [(entry.context, entry.msgid, entry.plural, comments)
            for entry, _line, comments in i18n.find_messages(text)]


class FindMessages(unittest.TestCase):
    def test_finds_plain_contextual_and_plural_macros(self):
        found = messages('tr!("Open"); trc!("button", "Save"); ntr!("{n} item", "{n} items", n);')
        self.assertEqual([m[:3] for m in found], [
            (None, "Open", None),
            ("button", "Save", None),
            (None, "{n} item", "{n} items"),
        ])

    def test_finds_marked_text_in_constant_tables(self):
        found = messages('const VALUES: &[(&str, &str)] = &[("unread", n_!("Unread"))];')
        self.assertEqual([m[:3] for m in found], [(None, "Unread", None)])

    def test_finds_literal_after_a_comment_inside_the_call(self):
        text = (
            "Some(trf!(\n"
            "    // TRANSLATORS: joins an hours part and a minutes part.\n"
            '    "{hours} {minutes}",\n'
            "    hours = h,\n"
            "))\n"
        )
        self.assertEqual(messages(text), [
            (None, "{hours} {minutes}", None, ["TRANSLATORS: joins an hours part and a minutes part."]),
        ])

    def test_keeps_translator_comment_above_the_call(self):
        text = '// TRANSLATORS: a menu item.\nlabel: trs!("_Open"),\n'
        self.assertEqual(messages(text), [(None, "_Open", None, ["TRANSLATORS: a menu item."])])

    def test_ignores_comments_without_the_translators_tag(self):
        text = '// just a note\ntr!(\n    // another note\n    "Close")\n'
        self.assertEqual(messages(text), [(None, "Close", None, [])])

    def test_extract_reads_a_source_tree(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "src").mkdir()
            (root / "src" / "a.rs").write_text('tr!(/* note */ "Hello")\n', encoding="utf-8")
            entries = i18n.extract([root / "src"], root)
        self.assertEqual([(e.msgid, e.refs) for e in entries], [("Hello", ["src/a.rs:1"])])


    def test_rejects_line_continuation_inside_a_translated_literal(self):
        with self.assertRaises(ValueError):
            messages('tr!("first part \\\n    second part")')

class StrftimeCheck(unittest.TestCase):
    def test_accepts_patterns_glib_can_format(self):
        self.assertEqual(i18n.strftime_problems("%A %-d %B %Y, %H:%M %% %Od %_e"), [])

    def test_rejects_unknown_conversion_and_lone_percent(self):
        self.assertEqual(len(i18n.strftime_problems("%A %Q")), 1)
        self.assertEqual(len(i18n.strftime_problems("%A %-d %B %")), 1)

    def test_check_reports_bad_translated_pattern_as_error(self):
        entry = i18n.Entry("%A %-d %B", context="strftime pattern: weekday, day, month")
        entry.msgstr = "%A %-d %B %Q"
        kinds = [kind for kind, _ in i18n.problems([entry], "fr")]
        self.assertEqual(kinds, ["error"])

    def test_plural_form_may_add_the_count_but_must_keep_other_names(self):
        entry = i18n.Entry("Message moved to {folder}", plural="{n} messages moved to {folder}")
        entry.msgstr_plural = ["Message déplacé vers {folder}", "{n} messages déplacés vers {folder}"]
        self.assertEqual(i18n.problems([entry], "fr"), [])
        entry.msgstr_plural = ["Message déplacé", "{n} messages déplacés"]
        self.assertEqual([kind for kind, _ in i18n.problems([entry], "fr")], ["error", "error"])

    def test_check_ignores_percent_in_ordinary_text(self):
        entry = i18n.Entry("100% done")
        entry.msgstr = "100 % fait"
        self.assertEqual(i18n.problems([entry], "fr"), [])


if __name__ == "__main__":
    unittest.main()
