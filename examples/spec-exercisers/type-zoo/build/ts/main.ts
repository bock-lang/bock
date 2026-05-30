function primitives(): void {
  const i: number = 42;
  const f: number = 3.14;
  const b: boolean = true;
  const s: string = "hello";
  const c: Char = 'A';
  const sum = (i + 8);
  const product = (f * 2.0);
  const power = (2 ** 10);
  const check = ((i > 0) && b);
  const either = ((i === 42) || (f < 1.0));
  console.log(`int=${i} float=${f} bool=${b} char=${c}`);
  return console.log(`sum=${sum} product=${product} power=${power}`);
}

export function identity<T>(x: T): T {
  return x;
}

export function firstOf<A, B>(a: A, b: B): A {
  return a;
}

export class Pair<A, B> {
  first: A;
  second: B;
  constructor({ first, second }: { first: A; second: B }) {
    this.first = first;
    this.second = second;
  }
}

// impl Pair
Pair.prototype.swap = function(self): Pair<B, A> {
  return new Pair({ first: self.second, second: self.first });
};

export class Box<T> {
  value: T;
  constructor({ value }: { value: T }) {
    this.value = value;
  }
}

// impl Box
Box.prototype.map = function<U>(self, f: (arg0: T) => U): Box<U> {
  return new Box({ value: f(self.value) });
};

export function maxOf<T extends Comparable>(a: T, b: T): T {
  return ((a > b) ? a : b);
}

export interface Describable {
  describe(self): string;
}

export class Color {
  r: number;
  g: number;
  b: number;
  constructor({ r, g, b }: { r: number; g: number; b: number }) {
    this.r = r;
    this.g = g;
    this.b = b;
  }
}

interface Color extends Describable {}
// impl Describable for Color
Color.prototype.describe = function(self): string {
  return `rgb(${self.r}, ${self.g}, ${self.b})`;
};

export function apply(f: (arg0: number) => number, x: number): number {
  return f(x);
}

export function applyTwice(f: (arg0: number) => number, x: number): number {
  return f(f(x));
}

export function composeInt(f: (arg0: number) => number, g: (arg0: number) => number): (arg0: number) => number {
  return (x) => f(g(x));
}

export type UserId = string;

export type Predicate = (arg0: number) => boolean;

export type StringPair = Pair<string, string>;

export function findUser(id: UserId): Optional<UserId> {
  return ((id === "") ? { _tag: "None" as const } : { _tag: "Some" as const, _0: id });
}

export function countMatching(items: Array<number>, pred: Predicate): number {
  return items.filter(items, pred).len(items.filter(items, pred));
}

export function describeOptional(opt: Optional<number>): string {
  return (() => {
    switch (opt._tag) {
      case "Some": {
        const n = opt._0;
        if (!((n > 0))) break;
        return `positive: ${n}`;
        break;
      }
      case "Some": {
        const n = opt._0;
        return `non-positive: ${n}`;
        break;
      }
      case "None": {
        return "absent";
        break;
      }
    }
  })();
}

export function safeDivide(a: number, b: number): Result<number, string> {
  return ((b === 0.0) ? { _tag: "Err" as const, _0: "division by zero" } : { _tag: "Ok" as const, _0: (a / b) });
}

export function chainedDivide(x: number): Result<number, string> {
  const half = safeDivide(x, 2.0);
  const quarter = safeDivide(half, 2.0);
  return { _tag: "Ok" as const, _0: quarter };
}

export function stats(items: Array<number>): [number, number] {
  const count = items.len(items);
  const total = items.len(items);
  return [count, total];
}

export function collectionsDemo(): void {
  const list = [10, 20, 30, 40, 50];
  const map = new Map([["name", "Bock"], ["version", "0.1"]]);
  const set = new Set(["alpha", "beta", "gamma"]);
  const n = list.len(list);
  const keys = map.keys(map);
  const has = set.len(set);
  console.log(`list len=${n} map keys=${keys} set size=${has}`);
  const extended = (list + [60, 70]);
  return console.log(`extended len=${extended.len(extended)}`);
}

export function chainDemo(): void {
  const numbers = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
  const result = numbers.filter(numbers, (n) => ((n % 2) === 0)).map(numbers.filter(numbers, (n) => ((n % 2) === 0)), (n) => (n * 2));
  return console.log(`chained result len=${result.len(result)}`);
}

export function double(x: number): number {
  return (x * 2);
}

export function increment(x: number): number {
  return (x + 1);
}

export function pipeDemo(): void {
  const piped = double(5);
  const transform = (composeX) => increment(double(composeX));
  return console.log(`piped=${piped}`);
}

function main() {
  console.log("=== Type Zoo ===");
  primitives();
  const n = identity(42);
  const s = identity("hello");
  const f = firstOf(1, "two");
  console.log(`identity(42)=${n} identity(hello)=${s} first_of=${f}`);
  const pair = new Pair({ first: 1, second: "one" });
  console.log(`pair: ${pair.first}, ${pair.second}`);
  const swapped = pair.swap(pair);
  console.log(`swapped: ${swapped.first}, ${swapped.second}`);
  const bigger = maxOf(10, 20);
  console.log(`max_of(10,20)=${bigger}`);
  const color = new Color({ r: 255, g: 128, b: 0 });
  console.log(`color: ${color.describe(color)}`);
  const doubled = apply((x) => (x * 2), 21);
  const quad = applyTwice((x) => (x * 2), 3);
  console.log(`apply doubled=${doubled} apply_twice=${quad}`);
  const user = findUser("alice");
  const evens = countMatching([1, 2, 3, 4, 5], (x) => ((x % 2) === 0));
  console.log(`find_user=some evens=${evens}`);
  console.log(describeOptional({ _tag: "Some" as const, _0: 42 }));
  console.log(describeOptional({ _tag: "None" as const }));
  const divResult = chainedDivide(100.0);
  switch (divResult._tag) {
    case "Ok": {
      const v = divResult._0;
      return console.log(`chained_divide(100)=${v}`);
      break;
    }
    case "Err": {
      const e = divResult._0;
      return console.log(`error: ${e}`);
      break;
    }
  }
  const [count, total] = stats([1, 2, 3]);
  console.log(`stats: count=${count} total=${total}`);
  collectionsDemo();
  chainDemo();
  return pipeDemo();
}
main();
//# sourceMappingURL=main.ts.map
