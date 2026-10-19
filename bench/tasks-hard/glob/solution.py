def match(pattern, path):
    """Full-match `path` against a glob `pattern`:
      *      any run of characters except '/' (possibly empty)
      **     any run of characters including '/' (possibly empty)
      ?      exactly one character except '/'
      [abc]  one character from the set; ranges like [a-z]; [!abc] negates;
             a ']' right after '[' or '[!' is a literal member; sets never match '/'
      \x     the character x literally (escape)
    Every other character matches itself. An unclosed '[' is a literal '['.
    Do not use fnmatch, glob or re."""
    raise NotImplementedError
