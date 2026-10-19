def parse_duration(s):
    """Parse a duration like '1h30m15s', '90s', '2d', '45m' into whole seconds (int).
    Units: d=86400, h=3600, m=60, s=1. Units must appear in descending order, each at most
    once, with a non-negative integer before each. Surrounding whitespace is ignored.
    Raise ValueError for anything else (empty string, no unit, unknown unit, repeated or
    out-of-order units, negative or fractional numbers)."""
    raise NotImplementedError
