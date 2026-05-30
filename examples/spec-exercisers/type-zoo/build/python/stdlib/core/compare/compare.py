from dataclasses import dataclass

@dataclass(frozen=True)
class Ordering_Less:
    _tag: str = "Less"

@dataclass(frozen=True)
class Ordering_Equal:
    _tag: str = "Equal"

@dataclass(frozen=True)
class Ordering_Greater:
    _tag: str = "Greater"

# trait Equatable
class Equatable:
    def eq(self, self, other: Self) -> bool:
        pass

# trait Comparable
class Comparable:
    def compare(self, self, other: Self) -> Ordering:
        pass

@dataclass
class Key(Comparable, Equatable):
    value: int

    def compare(self, self, other: Key) -> Ordering:
        return (Less if (self.value < other.value) else (Equal if (self.value == other.value) else Greater))

    def eq(self, self, other: Key) -> bool:
        return (self.value == other.value)

def key(value: int) -> Key:
    return Key(value=value)

def max(a: T, b: T) -> T:
    return (lambda __v: a if False else b)(a.compare(a, b))

def min(a: T, b: T) -> T:
    return (lambda __v: a if False else b)(a.compare(a, b))
