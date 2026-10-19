def split(total, weights):
    """Split an integer amount of cents `total` (may be negative) among len(weights) parties
    in proportion to non-negative integer `weights`, so that the parts are integers that sum
    to exactly `total`. Method: largest remainder. Each part starts as the truncation toward
    zero of total*w/sum(weights); the cents left over go one by one to the parties with the
    largest fractional remainder (by absolute value); ties go to the lower index. A zero
    weight always gets 0. Raise ValueError if weights is empty, any weight is negative, or
    all weights are zero. Use exact integer arithmetic (no floats)."""
    raise NotImplementedError
