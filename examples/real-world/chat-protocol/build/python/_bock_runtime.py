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

# ── Bock concurrency runtime ──
import asyncio as __bock_asyncio

class __BockChannel:
    __slots__ = ('_q',)
    def __init__(self):
        self._q = __bock_asyncio.Queue()
    def send(self, v):
        self._q.put_nowait(v)
    async def recv(self):
        return await self._q.get()
    def close(self):
        pass

def __bock_channel_new():
    ch = __BockChannel()
    return (ch, ch)

def __bock_spawn(x):
    # If already a coroutine, wrap it in a Task so it starts eagerly.
    if __bock_asyncio.iscoroutine(x):
        return __bock_asyncio.create_task(x)
    return x

__all__ = ["_BockSome", "_BockNone", "_bock_none", "_BockOk", "_BockErr", "__BockChannel", "__bock_channel_new", "__bock_spawn"]
