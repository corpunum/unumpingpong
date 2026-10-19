def order(deps):
    """Topological order of a build graph. `deps` maps each node to the list of nodes it
    depends on (they must come first). Nodes that only appear as dependencies are nodes
    too. Among the nodes that are ready at any point, the lexicographically smallest goes
    first (so the result is unique). A node listed twice as a dependency counts once.
    If there is a cycle, raise ValueError("cycle: " + ", ".join(sorted(stuck))) where
    `stuck` is every node that cannot be ordered (on a cycle or depending on one)."""
    raise NotImplementedError
