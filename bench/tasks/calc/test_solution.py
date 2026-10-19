import unittest
from solution import evaluate as e

class T(unittest.TestCase):
    def test_ok(self):
        self.assertEqual(e("1+2*3"), 7.0)
        self.assertEqual(e("(1+2)*3"), 9.0)
        self.assertEqual(e("10/4"), 2.5)
        self.assertEqual(e("8-3-2"), 3.0)
        self.assertEqual(e("2*-3"), -6.0)
        self.assertEqual(e("-(2+3)"), -5.0)
        self.assertEqual(e(" 3.5 * 2 "), 7.0)
        self.assertEqual(e("--4"), 4.0)
        self.assertEqual(e("2*(3+(4-1))/3"), 4.0)
    def test_bad(self):
        for s in ["", "1+", "(1", "1)", "1 2", "a+1", "*2", "()"]:
            with self.assertRaises(ValueError, msg=repr(s)):
                e(s)
        with self.assertRaises(ZeroDivisionError):
            e("1/(2-2)")
