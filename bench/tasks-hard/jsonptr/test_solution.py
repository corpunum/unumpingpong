import unittest
from solution import resolve

DOC = {"foo": ["bar", "baz"], "": 0, "a/b": 1, "c%d": 2, "e^f": 3, "g|h": 4,
       "i\\j": 5, "k\"l": 6, " ": 7, "m~n": 8, "~1": 9, "n": {"list": [[10, 11]]}}

class T(unittest.TestCase):
    def test_rfc_examples(self):
        self.assertEqual(resolve(DOC, ""), DOC)
        self.assertEqual(resolve(DOC, "/foo"), ["bar", "baz"])
        self.assertEqual(resolve(DOC, "/foo/0"), "bar")
        self.assertEqual(resolve(DOC, "/"), 0)
        self.assertEqual(resolve(DOC, "/a~1b"), 1)
        self.assertEqual(resolve(DOC, "/c%d"), 2)
        self.assertEqual(resolve(DOC, "/m~0n"), 8)
        self.assertEqual(resolve(DOC, "/ "), 7)
        self.assertEqual(resolve(DOC, "/n/list/0/1"), 11)
    def test_escape_order(self):
        self.assertEqual(resolve(DOC, "/~01"), 9)
    def test_errors(self):
        with self.assertRaises(ValueError):
            resolve(DOC, "foo")
        with self.assertRaises(ValueError):
            resolve(DOC, "/m~2n")
        with self.assertRaises(ValueError):
            resolve(DOC, "/x~")
        with self.assertRaises(KeyError):
            resolve(DOC, "/nope")
        for bad in ["01", "-1", "+1", " 1", "1.0"]:
            with self.assertRaises(ValueError, msg=bad):
                resolve(DOC, "/foo/" + bad)
        with self.assertRaises(IndexError):
            resolve(DOC, "/foo/-")
        with self.assertRaises(IndexError):
            resolve(DOC, "/foo/2")
        with self.assertRaises(TypeError):
            resolve(DOC, "/foo/0/x")
        with self.assertRaises(TypeError):
            resolve({"a": None}, "/a/b")

if __name__ == "__main__":
    unittest.main()
