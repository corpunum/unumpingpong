def compare_versions(a, b):
    """Compare two semantic version strings per semver 2.0 precedence.
    Format: MAJOR.MINOR.PATCH with an optional -prerelease (dot-separated identifiers) and an
    optional +build suffix (ignored). Return -1, 0 or 1 (a<b, a==b, a>b).
    A version with a prerelease is lower than the same version without one.
    Prerelease identifiers compare left to right: numeric identifiers numerically, others
    lexically in ASCII order, numeric < non-numeric, and a longer list wins if all earlier
    identifiers are equal. Raise ValueError for malformed versions."""
    raise NotImplementedError
