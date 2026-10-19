def resolve(doc, pointer):
    """Resolve an RFC 6901 JSON Pointer against `doc` (dicts, lists, scalars).
    - "" is the whole document. Otherwise the pointer must start with '/', else ValueError.
    - Reference tokens are separated by '/'; in a token '~1' means '/' and '~0' means '~'
      (decode ~1 first is WRONG: '~01' is the key '~1'). '~' followed by anything else is ValueError.
    - On a dict: the token is the key (missing key: KeyError).
    - On a list: the token must be '0' or a decimal without leading zeros ('01', '-1', '+1',
      ' 1', '1.0' are ValueError); '-' is IndexError (it names the element after the end);
      out of range is IndexError.
    - On a scalar (str, int, None, ...): TypeError."""
    raise NotImplementedError
