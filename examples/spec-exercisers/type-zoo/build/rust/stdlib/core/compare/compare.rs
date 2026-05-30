#![allow(unused_variables, unused_imports, unused_parens, dead_code, non_upper_case_globals)]

pub enum Ordering {
    Less,
    Equal,
    Greater,
}

pub trait Equatable {
    fn eq(&self, self: _, other: Self) -> bool;
}

pub trait Comparable {
    fn compare(&self, self: _, other: Self) -> Ordering;
}

pub struct Key {
    pub value: i64,
}

impl Comparable for Key {
    fn compare(&self, self: _, other: Key) -> Ordering {
        if (self.value < other.value) { Less } else { if (self.value == other.value) { Equal } else { Greater } }
    }
}

impl Equatable for Key {
    fn eq(&self, self: _, other: Key) -> bool {
        (self.value == other.value)
    }
}

pub fn key(value: i64) -> Key {
    Key { value: value }
}

pub fn max<T: Comparable>(a: T, b: T) -> T {
    match a.compare(a, b) {
        Greater => a,
        _ => b,
    }
}

pub fn min<T: Comparable>(a: T, b: T) -> T {
    match a.compare(a, b) {
        Less => a,
        _ => b,
    }
}
