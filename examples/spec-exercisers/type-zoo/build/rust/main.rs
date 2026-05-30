#![allow(unused_variables, unused_imports, unused_parens, dead_code, non_upper_case_globals)]

fn primitives() -> () {
    let i: i64 = 42_i64;
    let f: f64 = 3.14_f64;
    let b: bool = true;
    let s: String = "hello".to_string();
    let c: Char = 'A';
    let sum = (i + 8_i64);
    let product = (f * 2.0_f64);
    let power = (2_i64.pow(10_i64));
    let check = ((i > 0_i64) && b);
    let either = ((i == 42_i64) || (f < 1.0_f64));
    println!("{}", format!("int={} float={} bool={} char={}", i, f, b, c));
    println!("{}", format!("sum={} product={} power={}", sum, product, power))
}

pub fn identity<T>(x: T) -> T {
    x
}

pub fn first_of<A, B>(a: A, b: B) -> A {
    a
}

pub struct Pair<A, B> {
    pub first: A,
    pub second: B,
}

impl Pair {
    pub fn swap(&self, self: _) -> Pair<B, A> {
        Pair { first: self.second, second: self.first }
    }
}

pub struct Box<T> {
    pub value: T,
}

impl Box {
    pub fn map<U>(&self, self: _, f: fn(T) -> U) -> Box<U> {
        Box { value: f(self.value) }
    }
}

pub fn max_of<T: Comparable>(a: T, b: T) -> T {
    if (a > b) { a } else { b }
}

pub trait Describable {
    fn describe(&self, self: _) -> String;
}

pub struct Color {
    pub r: i64,
    pub g: i64,
    pub b: i64,
}

impl Describable for Color {
    fn describe(&self, self: _) -> String {
        format!("rgb({}, {}, {})", self.r, self.g, self.b)
    }
}

pub fn apply(f: fn(i64) -> i64, x: i64) -> i64 {
    f(x)
}

pub fn apply_twice(f: fn(i64) -> i64, x: i64) -> i64 {
    f(f(x))
}

pub fn compose_int(f: fn(i64) -> i64, g: fn(i64) -> i64) -> fn(i64) -> i64 {
    |x: _| f(g(x))
}

pub type UserId = String;

pub type Predicate = fn(i64) -> bool;

pub type StringPair = Pair<String, String>;

pub fn find_user(id: UserId) -> Option<UserId> {
    if (id == "".to_string()) { None } else { Some(id) }
}

pub fn count_matching(items: Vec<i64>, pred: Predicate) -> i64 {
    items.filter(items, pred).len(items.filter(items, pred))
}

pub fn describe_optional(opt: Option<i64>) -> String {
    match opt {
        Some(n) if (n > 0_i64) => format!("positive: {}", n),
        Some(n) => format!("non-positive: {}", n),
        None => "absent".to_string(),
    }
}

pub fn safe_divide(a: f64, b: f64) -> Result<f64, String> {
    if (b == 0.0_f64) { Err("division by zero".to_string()) } else { Ok((a / b)) }
}

pub fn chained_divide(x: f64) -> Result<f64, String> {
    let half = safe_divide(x, 2.0_f64)?;
    let quarter = safe_divide(half, 2.0_f64)?;
    Ok(quarter)
}

pub fn stats(items: Vec<i64>) -> (i64, i64) {
    let count = items.len(items);
    let total = items.len(items);
    (count, total)
}

pub fn collections_demo() -> () {
    let list = vec![10_i64, 20_i64, 30_i64, 40_i64, 50_i64];
    let map = std::collections::HashMap::from([("name".to_string(), "Bock".to_string()), ("version".to_string(), "0.1".to_string())]);
    let set = std::collections::HashSet::from(["alpha".to_string(), "beta".to_string(), "gamma".to_string()]);
    let n = list.len(list);
    let keys = map.keys(map);
    let has = set.len(set);
    println!("{}", format!("list len={} map keys={} set size={}", n, keys, has));
    let extended = (list + vec![60_i64, 70_i64]);
    println!("{}", format!("extended len={}", extended.len(extended)))
}

pub fn chain_demo() -> () {
    let numbers = vec![1_i64, 2_i64, 3_i64, 4_i64, 5_i64, 6_i64, 7_i64, 8_i64, 9_i64, 10_i64];
    let result = numbers.filter(numbers, |n: _| ((n % 2_i64) == 0_i64)).map(numbers.filter(numbers, |n: _| ((n % 2_i64) == 0_i64)), |n: _| (n * 2_i64));
    println!("{}", format!("chained result len={}", result.len(result)))
}

pub fn double(x: i64) -> i64 {
    (x * 2_i64)
}

pub fn increment(x: i64) -> i64 {
    (x + 1_i64)
}

pub fn pipe_demo() -> () {
    let piped = double(5_i64);
    let transform = |__compose_x: _| increment(double(__compose_x));
    println!("{}", format!("piped={}", piped))
}

fn main() {
    println!("{}", "=== Type Zoo ===".to_string());
    primitives();
    let n = identity(42_i64);
    let s = identity("hello".to_string());
    let f = first_of(1_i64, "two".to_string());
    println!("{}", format!("identity(42)={} identity(hello)={} first_of={}", n, s, f));
    let pair = Pair { first: 1_i64, second: "one".to_string() };
    println!("{}", format!("pair: {}, {}", pair.first, pair.second));
    let swapped = pair.swap(pair);
    println!("{}", format!("swapped: {}, {}", swapped.first, swapped.second));
    let bigger = max_of(10_i64, 20_i64);
    println!("{}", format!("max_of(10,20)={}", bigger));
    let color = Color { r: 255_i64, g: 128_i64, b: 0_i64 };
    println!("{}", format!("color: {}", color.describe(color)));
    let doubled = apply(|x: _| (x * 2_i64), 21_i64);
    let quad = apply_twice(|x: _| (x * 2_i64), 3_i64);
    println!("{}", format!("apply doubled={} apply_twice={}", doubled, quad));
    let user = find_user("alice".to_string());
    let evens = count_matching(vec![1_i64, 2_i64, 3_i64, 4_i64, 5_i64], |x: _| ((x % 2_i64) == 0_i64));
    println!("{}", format!("find_user=some evens={}", evens));
    println!("{}", describe_optional(Some(42_i64)));
    println!("{}", describe_optional(None));
    let div_result = chained_divide(100.0_f64);
    match div_result {
        Ok(v) => println!("{}", format!("chained_divide(100)={}", v)),
        Err(e) => println!("{}", format!("error: {}", e)),
    }
    let (count, total) = stats(vec![1_i64, 2_i64, 3_i64]);
    println!("{}", format!("stats: count={} total={}", count, total));
    collections_demo();
    chain_demo();
    pipe_demo()
}
