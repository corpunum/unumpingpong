import unittest
from solution import match

class T(unittest.TestCase):
    def test_star(self):
        self.assertTrue(match("*.py", "a.py"))
        self.assertTrue(match("*.py", ".py"))
        self.assertFalse(match("*.py", "src/a.py"))
        self.assertTrue(match("src/*/a", "src/x/a"))
        self.assertFalse(match("src/*/a", "src/x/y/a"))
    def test_globstar(self):
        self.assertTrue(match("**.py", "src/x/a.py"))
        self.assertTrue(match("src/**/a", "src/x/y/a"))
        self.assertTrue(match("src/**", "src/"))
        self.assertTrue(match("**", ""))
    def test_question(self):
        self.assertTrue(match("a?c", "abc"))
        self.assertFalse(match("a?c", "a/c"))
        self.assertFalse(match("a?c", "ac"))
    def test_sets(self):
        self.assertTrue(match("[abc]x", "bx"))
        self.assertTrue(match("[a-c]x", "bx"))
        self.assertFalse(match("[!a-c]x", "bx"))
        self.assertTrue(match("[!a-c]x", "dx"))
        self.assertTrue(match("[]]", "]"))
        self.assertTrue(match("[!]]", "a"))
        self.assertFalse(match("[!a]", "/"))
        self.assertTrue(match("[a-]", "-"))
        self.assertTrue(match("[", "["))
        self.assertTrue(match("a[b", "a[b"))
    def test_escape_and_full_match(self):
        self.assertTrue(match("\\*", "*"))
        self.assertFalse(match("\\*", "x"))
        self.assertTrue(match("a\\?", "a?"))
        self.assertFalse(match("abc", "abcd"))
        self.assertFalse(match("a*", "ba"))
    def test_no_library(self):
        src = open(__import__("solution").__file__).read()
        for word in ["fnmatch", "import re", "from re", "import glob"]:
            self.assertNotIn(word, src)

if __name__ == "__main__":
    unittest.main()
