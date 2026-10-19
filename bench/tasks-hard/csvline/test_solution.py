import unittest
from solution import parse_line

class T(unittest.TestCase):
    def test_plain(self):
        self.assertEqual(parse_line("a,b,c"), ["a", "b", "c"])
    def test_empty_line_and_fields(self):
        self.assertEqual(parse_line(""), [""])
        self.assertEqual(parse_line(","), ["", ""])
        self.assertEqual(parse_line("a,,b,"), ["a", "", "b", ""])
    def test_quotes(self):
        self.assertEqual(parse_line('"a,b",c'), ["a,b", "c"])
        self.assertEqual(parse_line('"say ""hi""",x'), ['say "hi"', "x"])
        self.assertEqual(parse_line('""'), [""])
        self.assertEqual(parse_line('"",""'), ["", ""])
        self.assertEqual(parse_line('"line1\nline2"'), ["line1\nline2"])
        self.assertEqual(parse_line('a,""""'), ["a", '"'])
    def test_spaces_are_kept(self):
        self.assertEqual(parse_line(" a , b "), [" a ", " b "])
        self.assertEqual(parse_line(' "a"'), [' "a"'])
    def test_errors(self):
        for bad in ['"abc', '"a"b,c', 'a"b', '"a""', 'x,"y" ,z']:
            with self.assertRaises(ValueError, msg=bad):
                parse_line(bad)

if __name__ == "__main__":
    unittest.main()
