import unittest
from solution import to_roman, from_roman

class T(unittest.TestCase):
    def test_examples(self):
        for n, r in [(1, "I"), (4, "IV"), (9, "IX"), (14, "XIV"), (40, "XL"), (90, "XC"),
                     (400, "CD"), (1994, "MCMXCIV"), (2024, "MMXXIV"), (3999, "MMMCMXCIX")]:
            self.assertEqual(to_roman(n), r)
            self.assertEqual(from_roman(r), n)
    def test_round_trip(self):
        for n in range(1, 4000):
            self.assertEqual(from_roman(to_roman(n)), n)
    def test_range_and_types(self):
        for bad in [0, -1, 4000]:
            with self.assertRaises(ValueError):
                to_roman(bad)
        for bad in [1.0, "5", True]:
            with self.assertRaises(TypeError):
                to_roman(bad)
    def test_strict_parse(self):
        for bad in ["", "IIII", "VV", "IC", "IL", "XM", "MMMM", "iv", " IV", "IIV", "VX", "LC", "DM", "XXXX", "CCCC", "IXI"]:
            with self.assertRaises(ValueError, msg=bad):
                from_roman(bad)

if __name__ == "__main__":
    unittest.main()
