function primitives() {
  const i = 42;
  const f = 3.14;
  const b = true;
  const s = "hello";
  const c = 'A';
  const sum = (i + 8);
  const product = (f * 2.0);
  const power = (2 ** 10);
  const check = ((i > 0) && b);
  const either = ((i === 42) || (f < 1.0));
  console.log(`int=${i} float=${f} bool=${b} char=${c}`);
  return console.log(`sum=${sum} product=${product} power=${power}`);
}

export function identity(x) {
  return x;
}

export function firstOf(a, b) {
  return a;
}

class Pair {
  constructor({ first, second }) {
    this.first = first;
    this.second = second;
  }
}

// impl Pair
Pair.prototype.swap = function(self) {
  return new Pair({ first: self.second, second: self.first });
};

class Box {
  constructor({ value }) {
    this.value = value;
  }
}

// impl Box
Box.prototype.map = function(self, f) {
  return new Box({ value: f(self.value) });
};

export function maxOf(a, b) {
  return ((a > b) ? a : b);
}

// trait Describable
const Describable = {
  describe(self) {
  },
};

class Color {
  constructor({ r, g, b }) {
    this.r = r;
    this.g = g;
    this.b = b;
  }
}

// impl Describable for Color
Color.prototype.describe = function(self) {
  return `rgb(${self.r}, ${self.g}, ${self.b})`;
};

export function apply(f, x) {
  return f(x);
}

export function applyTwice(f, x) {
  return f(f(x));
}

export function composeInt(f, g) {
  return (x) => f(g(x));
}

// type UserId = ...

// type Predicate = ...

// type StringPair = ...

export function findUser(id) {
  return ((id === "") ? { _tag: "None" } : { _tag: "Some", _0: id });
}

export function countMatching(items, pred) {
  return items.filter(items, pred).len(items.filter(items, pred));
}

export function describeOptional(opt) {
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

export function safeDivide(a, b) {
  return ((b === 0.0) ? { _tag: "Err", _0: "division by zero" } : { _tag: "Ok", _0: (a / b) });
}

export function chainedDivide(x) {
  const half = safeDivide(x, 2.0);
  const quarter = safeDivide(half, 2.0);
  return { _tag: "Ok", _0: quarter };
}

export function stats(items) {
  const count = items.len(items);
  const total = items.len(items);
  return [count, total];
}

export function collectionsDemo() {
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

export function chainDemo() {
  const numbers = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
  const result = numbers.filter(numbers, (n) => ((n % 2) === 0)).map(numbers.filter(numbers, (n) => ((n % 2) === 0)), (n) => (n * 2));
  return console.log(`chained result len=${result.len(result)}`);
}

export function double(x) {
  return (x * 2);
}

export function increment(x) {
  return (x + 1);
}

export function pipeDemo() {
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
  console.log(describeOptional({ _tag: "Some", _0: 42 }));
  console.log(describeOptional({ _tag: "None" }));
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
//# sourceMappingURL=main.js.map
