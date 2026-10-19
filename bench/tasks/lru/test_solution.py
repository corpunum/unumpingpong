import unittest
from solution import LRUCache

class T(unittest.TestCase):
    def test_evict(self):
        c = LRUCache(2)
        c.put("a", 1); c.put("b", 2)
        self.assertEqual(c.get("a"), 1)
        c.put("c", 3)
        self.assertNotIn("b", c)
        self.assertEqual(c.keys(), ["a", "c"])
        self.assertEqual(len(c), 2)
    def test_update_and_default(self):
        c = LRUCache(2)
        c.put("a", 1); c.put("b", 2); c.put("a", 10)
        self.assertEqual(c.keys(), ["b", "a"])
        self.assertEqual(c.get("zz", 7), 7)
        self.assertIsNone(c.get("zz"))
        self.assertEqual(c.get("a"), 10)
    def test_contains_keeps_order(self):
        c = LRUCache(2)
        c.put("a", 1); c.put("b", 2)
        self.assertIn("a", c)
        self.assertEqual(c.keys(), ["a", "b"])
        c.put("c", 3)
        self.assertEqual(c.keys(), ["b", "c"])
    def test_capacity(self):
        for n in (0, -1):
            with self.assertRaises(ValueError):
                LRUCache(n)
        c = LRUCache(1)
        c.put(1, 1); c.put(2, 2)
        self.assertEqual(c.keys(), [2])
