from dataclasses import dataclass

def primitives() -> None:
    i: int = 42
    f: float = 3.14
    b: bool = True
    s: str = "hello"
    c: Char = 'A'
    sum = (i + 8)
    product = (f * 2.0)
    power = (2 ** 10)
    check = ((i > 0) and b)
    either = ((i == 42) or (f < 1.0))
    print(f"int={i} float={f} bool={b} char={c}")
    return print(f"sum={sum} product={product} power={power}")

def identity(x: T) -> T:
    return x

def first_of(a: A, b: B) -> A:
    return a

@dataclass
class Pair:
    first: A
    second: B

# impl Pair
def swap(self, self) -> Pair[B, A]:
    return Pair(first=self.second, second=self.first)

@dataclass
class Box:
    value: T

# impl Box
def map(self, self, f: Callable[[T], U]) -> Box[U]:
    return Box(value=f(self.value))

def max_of(a: T, b: T) -> T:
    return (a if (a > b) else b)

# trait Describable
class Describable:
    def describe(self, self) -> str:
        pass

@dataclass
class Color(Describable):
    r: int
    g: int
    b: int

    def describe(self, self) -> str:
        return f"rgb({self.r}, {self.g}, {self.b})"

def apply(f: Callable[[int], int], x: int) -> int:
    return f(x)

def apply_twice(f: Callable[[int], int], x: int) -> int:
    return f(f(x))

def compose_int(f: Callable[[int], int], g: Callable[[int], int]) -> Callable[[int], int]:
    return lambda x: f(g(x))

# type UserId = ...

# type Predicate = ...

# type StringPair = ...

def find_user(id: UserId) -> Optional[UserId]:
    return (None if (id == "") else Some(id))

def count_matching(items: list[int], pred: Predicate) -> int:
    return items.filter(items, pred).len(items.filter(items, pred))

def describe_optional(opt: Optional[int]) -> str:
    return (lambda __v: f"positive: {n}" if False else f"non-positive: {n}" if False else "absent")(opt)

def safe_divide(a: float, b: float) -> Result[float, str]:
    return (Err("division by zero") if (b == 0.0) else Ok((a / b)))

def chained_divide(x: float) -> Result[float, str]:
    half = safe_divide(x, 2.0)
    quarter = safe_divide(half, 2.0)
    return Ok(quarter)

def stats(items: list[int]) -> tuple[int, int]:
    count = items.len(items)
    total = items.len(items)
    return (count, total)

def collections_demo() -> None:
    list = [10, 20, 30, 40, 50]
    map = {"name": "Bock", "version": "0.1"}
    set = {"alpha", "beta", "gamma"}
    n = list.len(list)
    keys = map.keys(map)
    has = set.len(set)
    print(f"list len={n} map keys={keys} set size={has}")
    extended = (list + [60, 70])
    return print(f"extended len={extended.len(extended)}")

def chain_demo() -> None:
    numbers = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    result = numbers.filter(numbers, lambda n: ((n % 2) == 0)).map(numbers.filter(numbers, lambda n: ((n % 2) == 0)), lambda n: (n * 2))
    return print(f"chained result len={result.len(result)}")

def double(x: int) -> int:
    return (x * 2)

def increment(x: int) -> int:
    return (x + 1)

def pipe_demo() -> None:
    piped = double(5)
    transform = lambda __compose_x: increment(double(__compose_x))
    return print(f"piped={piped}")

def main():
    print("=== Type Zoo ===")
    primitives()
    n = identity(42)
    s = identity("hello")
    f = first_of(1, "two")
    print(f"identity(42)={n} identity(hello)={s} first_of={f}")
    pair = Pair(first=1, second="one")
    print(f"pair: {pair.first}, {pair.second}")
    swapped = pair.swap(pair)
    print(f"swapped: {swapped.first}, {swapped.second}")
    bigger = max_of(10, 20)
    print(f"max_of(10,20)={bigger}")
    color = Color(r=255, g=128, b=0)
    print(f"color: {color.describe(color)}")
    doubled = apply(lambda x: (x * 2), 21)
    quad = apply_twice(lambda x: (x * 2), 3)
    print(f"apply doubled={doubled} apply_twice={quad}")
    user = find_user("alice")
    evens = count_matching([1, 2, 3, 4, 5], lambda x: ((x % 2) == 0))
    print(f"find_user=some evens={evens}")
    print(describe_optional(Some(42)))
    print(describe_optional(None))
    div_result = chained_divide(100.0)
    match div_result:
        case Ok(_0=v):
            return print(f"chained_divide(100)={v}")
        case Err(_0=e):
            return print(f"error: {e}")
    (count, total) = stats([1, 2, 3])
    print(f"stats: count={count} total={total}")
    collections_demo()
    chain_demo()
    return pipe_demo()
if __name__ == "__main__":
    main()
