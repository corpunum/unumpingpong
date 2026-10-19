def to_roman(n):
    """Integer 1..3999 to a canonical Roman numeral (e.g. 1994 -> 'MCMXCIV').
    Raise ValueError outside 1..3999 and TypeError for non-int (bool is not an int here)."""
    raise NotImplementedError


def from_roman(s):
    """Canonical Roman numeral to int. Strict: only the canonical form is accepted
    (from_roman(to_roman(n)) == n for every n, and every other string raises ValueError):
    no 'IIII', 'VV', 'IC', 'IL', 'XM', 'MMMM', lowercase, spaces or the empty string."""
    raise NotImplementedError
