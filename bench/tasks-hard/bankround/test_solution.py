import unittest
from solution import round_decimal as r

class T(unittest.TestCase):
    def test_half_even(self):
        self.assertEqual(r("2.5", 0), "2")
        self.assertEqual(r("3.5", 0), "4")
        self.assertEqual(r("0.125", 2), "0.12")
        self.assertEqual(r("0.135", 2), "0.14")
        self.assertEqual(r("1.005", 2), "1.00")
        self.assertEqual(r("1.0051", 2), "1.01")
        self.assertEqual(r("-1.235", 2), "-1.24")
        self.assertEqual(r("-2.5", 0), "-2")
    def test_carry_and_padding(self):
        self.assertEqual(r("9.995", 2), "10.00")
        self.assertEqual(r("99.5", 0), "100")
        self.assertEqual(r("123", 2), "123.00")
        self.assertEqual(r(".5", 0), "0")
        self.assertEqual(r("5.", 1), "5.0")
        self.assertEqual(r("+007.10", 1), "7.1")
        self.assertEqual(r("0.0004", 3), "0.000")
    def test_no_negative_zero(self):
        self.assertEqual(r("-0.4", 0), "0")
        self.assertEqual(r("-0.0", 2), "0.00")
        self.assertEqual(r("-0.005", 2), "0.00")
    def test_errors(self):
        for bad in ["", ".", "1e5", "1,5", " 1", "1.2.3", "-", "abc", "--1"]:
            with self.assertRaises(ValueError, msg=bad):
                r(bad, 1)
        with self.assertRaises(ValueError):
            r("1.5", -1)

if __name__ == "__main__":
    unittest.main()
