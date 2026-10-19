import unittest
from solution import compare_versions as c

class T(unittest.TestCase):
    def test_core(self):
        self.assertEqual(c("1.2.3", "1.2.3"), 0)
        self.assertEqual(c("1.2.3", "1.10.0"), -1)
        self.assertEqual(c("2.0.0", "1.99.99"), 1)
    def test_prerelease(self):
        self.assertEqual(c("1.0.0-alpha", "1.0.0"), -1)
        self.assertEqual(c("1.0.0-alpha", "1.0.0-alpha.1"), -1)
        self.assertEqual(c("1.0.0-alpha.1", "1.0.0-alpha.beta"), -1)
        self.assertEqual(c("1.0.0-beta.2", "1.0.0-beta.11"), -1)
        self.assertEqual(c("1.0.0-rc.1", "1.0.0-beta.11"), 1)
        self.assertEqual(c("1.0.0-1", "1.0.0-alpha"), -1)
    def test_build_ignored(self):
        self.assertEqual(c("1.0.0+a", "1.0.0+b"), 0)
        self.assertEqual(c("1.0.0-x+1", "1.0.0-x"), 0)
    def test_bad(self):
        for s in ["1.2", "a.b.c", "1.2.3.4", "", "1.2.3-", "01.2.3"]:
            with self.assertRaises(ValueError, msg=s):
                c(s, "1.0.0")
