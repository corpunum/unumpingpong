import unittest
from solution import parse_duration as p

class T(unittest.TestCase):
    def test_ok(self):
        self.assertEqual(p("1h30m15s"), 5415)
        self.assertEqual(p("90s"), 90)
        self.assertEqual(p("2d"), 172800)
        self.assertEqual(p(" 45m "), 2700)
        self.assertEqual(p("1d1s"), 86401)
        self.assertEqual(p("0s"), 0)
    def test_bad(self):
        for s in ["", "   ", "15", "1x", "1s1m", "1m1m", "-5s", "1.5h", "h", "1h 30m", "1h30"]:
            with self.assertRaises(ValueError, msg=repr(s)):
                p(s)
