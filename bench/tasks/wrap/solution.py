def wrap(text, width):
    """Greedy word-wrap `text` to lines of at most `width` characters; return a list of lines.
    Words are separated by any whitespace (runs collapse to a single space; leading and
    trailing whitespace is dropped). A word longer than `width` is split into chunks of
    `width` characters (the last chunk may be shorter and can be followed by more words on
    the same line if they fit). Empty/whitespace-only text gives []. Raise ValueError if
    width < 1."""
    raise NotImplementedError
