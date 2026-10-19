import unittest
from solution import wrap as w

class T(unittest.TestCase):
    def test_basic(self):
        self.assertEqual(w("the quick brown fox", 10), ["the quick", "brown fox"])
        self.assertEqual(w("  a   b \n c\t", 3), ["a b", "c"])
        self.assertEqual(w("", 5), [])
        self.assertEqual(w("   ", 5), [])
        self.assertEqual(w("abc def", 3), ["abc", "def"])
    def test_long_words(self):
        self.assertEqual(w("abcdefgh", 3), ["abc", "def", "gh"])
        self.assertEqual(w("abcde x yz", 4), ["abcd", "e x", "yz"])
        self.assertEqual(w("hello wonderful", 6), ["hello", "wonder", "ful"])
    def test_width(self):
        with self.assertRaises(ValueError):
            w("x", 0)
