def parse_line(line):
    """Parse ONE line of CSV per RFC 4180 and return the list of fields (strings).
    - Fields are separated by commas. An empty line is one empty field: [''].
    - A field may be quoted with double quotes; inside quotes, commas and newlines are
      literal and a doubled quote "" stands for one ".
    - Spaces are significant and kept as they are (no trimming), also around quoted fields
      ONLY if they are outside the quotes: a space before an opening quote makes the whole
      field unquoted text (so ' "a"' is the field ' "a"' with its quotes kept).
    - After a closing quote, only a comma or the end of the line may follow; anything else
      raises ValueError. An unterminated quoted field raises ValueError.
    - A bare quote inside an unquoted field (a"b) raises ValueError."""
    raise NotImplementedError
