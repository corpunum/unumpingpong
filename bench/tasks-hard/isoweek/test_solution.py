import datetime
import unittest
from solution import iso_week

class T(unittest.TestCase):
    def test_boundaries(self):
        self.assertEqual(iso_week(2021, 1, 1), (2020, 53, 5))
        self.assertEqual(iso_week(2019, 12, 30), (2020, 1, 1))
        self.assertEqual(iso_week(2026, 12, 31), (2026, 53, 4))
        self.assertEqual(iso_week(2027, 1, 3), (2026, 53, 7))
        self.assertEqual(iso_week(2000, 2, 29), (2000, 9, 2))
    def test_against_reference_for_many_dates(self):
        d = datetime.date(1999, 12, 1)
        while d < datetime.date(2031, 2, 1):
            self.assertEqual(iso_week(d.year, d.month, d.day), tuple(d.isocalendar()), d)
            d += datetime.timedelta(days=3)
    def test_invalid(self):
        for y, m, dd in [(2021, 2, 29), (2021, 13, 1), (2021, 0, 1), (2021, 4, 31), (2021, 1, 0)]:
            with self.assertRaises(ValueError):
                iso_week(y, m, dd)
    def test_no_library(self):
        src = open(__import__("solution").__file__).read()
        self.assertNotIn("datetime", src)
        self.assertNotIn("calendar", src)

if __name__ == "__main__":
    unittest.main()
