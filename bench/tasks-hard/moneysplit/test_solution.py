import unittest
from solution import split

class T(unittest.TestCase):
    def test_even_and_remainders(self):
        self.assertEqual(split(100, [1, 1, 1]), [34, 33, 33])
        self.assertEqual(split(10, [1, 2]), [3, 7])
        self.assertEqual(split(7, [1, 1, 1, 1]), [2, 2, 2, 1])
        self.assertEqual(split(101, [50, 30, 20]), [51, 30, 20])
    def test_largest_remainder_not_index(self):
        # 100 * [1, 2, 4] / 7 = 14.28, 28.57, 57.14 -> the .57 gets the cent.
        self.assertEqual(split(100, [1, 2, 4]), [14, 29, 57])
    def test_negative_total(self):
        self.assertEqual(split(-100, [1, 1, 1]), [-34, -33, -33])
        self.assertEqual(split(-100, [1, 2, 4]), [-14, -29, -57])
    def test_zero_weights_and_sum(self):
        self.assertEqual(split(5, [0, 1, 0, 1]), [0, 3, 0, 2])
        for total in range(-50, 51):
            for w in ([3, 1, 4, 1, 5], [9, 0, 2], [1]):
                self.assertEqual(sum(split(total, w)), total)
    def test_huge_exact(self):
        self.assertEqual(split(10**30 + 1, [1, 1]), [5 * 10**29 + 1, 5 * 10**29])
    def test_errors(self):
        for w in ([], [0, 0], [1, -1]):
            with self.assertRaises(ValueError):
                split(10, w)

if __name__ == "__main__":
    unittest.main()
