import unittest
from solution import merge_intervals as m

class T(unittest.TestCase):
    def test_basic(self):
        self.assertEqual(m([]), [])
        self.assertEqual(m([(1, 3), (2, 6), (8, 10), (15, 18)]), [(1, 6), (8, 10), (15, 18)])
        self.assertEqual(m([(1, 3), (3, 5)]), [(1, 5)])
        self.assertEqual(m([(1, 2), (3, 4)]), [(1, 2), (3, 4)])
    def test_unordered_and_nested(self):
        self.assertEqual(m([(5, 6), (1, 10), (2, 3)]), [(1, 10)])
        self.assertEqual(m([(4, 4), (1, 1)]), [(1, 1), (4, 4)])
    def test_no_mutation(self):
        a = [(3, 4), (1, 3)]
        b = list(a)
        m(a)
        self.assertEqual(a, b)
