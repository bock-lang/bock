# ── Bock Result runtime ──
class _BockOk:
    __match_args__ = ('_0',)
    __slots__ = ('_0',)
    def __init__(self, _0):
        self._0 = _0
    def __repr__(self):
        return f'Ok({self._0!r})'

class _BockErr:
    __match_args__ = ('_0',)
    __slots__ = ('_0',)
    def __init__(self, _0):
        self._0 = _0
    def __repr__(self):
        return f'Err({self._0!r})'

__all__ = ["_BockOk", "_BockErr"]
