class LRUCache:
    """Least-recently-used cache with a fixed capacity (>= 1; raise ValueError otherwise).
    get(key, default=None): return the value and mark the key most recently used, or default.
    put(key, value): insert or update (updating also marks it most recently used); when the
    size would exceed capacity, evict the least recently used key.
    __len__ gives the number of items, __contains__ must NOT change recency, and keys()
    returns the keys from least to most recently used. All operations are O(1) average."""
    def __init__(self, capacity):
        raise NotImplementedError
