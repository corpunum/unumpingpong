import unittest
from solution import order

class T(unittest.TestCase):
    def test_simple(self):
        self.assertEqual(order({"b": ["a"], "c": ["b"]}), ["a", "b", "c"])
        self.assertEqual(order({}), [])
    def test_lexicographic_ties(self):
        self.assertEqual(order({"z": [], "a": [], "m": []}), ["a", "m", "z"])
        self.assertEqual(order({"d": ["b", "c"], "b": ["a"], "c": ["a"]}), ["a", "b", "c", "d"])
        # Ready set changes: x unlocks a, which beats y.
        self.assertEqual(order({"a": ["x"], "y": [], "x": []}), ["x", "a", "y"])
    def test_implicit_nodes_and_duplicates(self):
        self.assertEqual(order({"app": ["lib", "lib", "util"]}), ["lib", "util", "app"])
    def test_cycle_names_every_stuck_node(self):
        with self.assertRaises(ValueError) as e:
            order({"a": ["b"], "b": ["a"], "c": ["a"], "d": []})
        self.assertEqual(str(e.exception), "cycle: a, b, c")
        with self.assertRaises(ValueError) as e:
            order({"s": ["s"]})
        self.assertEqual(str(e.exception), "cycle: s")

if __name__ == "__main__":
    unittest.main()
