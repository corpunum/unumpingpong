def iso_week(year, month, day):
    """ISO 8601 week date of a Gregorian date: return (iso_year, week, weekday) with
    weekday 1 = Monday .. 7 = Sunday. Week 1 is the week with the year's first Thursday,
    so early January can belong to the previous ISO year and late December to the next.
    Raise ValueError for an invalid date (month 13, Feb 29 in a non-leap year, day 0...).
    Do not use the datetime or calendar modules: compute it."""
    raise NotImplementedError
