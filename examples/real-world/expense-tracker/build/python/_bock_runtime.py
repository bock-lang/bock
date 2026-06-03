# ── Bock Optional runtime ──
class _BockSome:
    __match_args__ = ('_0',)
    __slots__ = ('_0',)
    def __init__(self, _0):
        self._0 = _0
    def __repr__(self):
        return f'Some({self._0!r})'

class _BockNone:
    __slots__ = ()
    def __repr__(self):
        return 'None'

_bock_none = _BockNone()

__all__ = ["_BockSome", "_BockNone", "_bock_none"]
