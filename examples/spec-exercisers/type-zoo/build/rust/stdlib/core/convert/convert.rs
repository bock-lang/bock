#![allow(unused_variables, unused_imports, unused_parens, dead_code, non_upper_case_globals)]

pub trait From<T> {
    fn from(&self, value: T) -> Self;
}

pub trait Into<T> {
    fn into(&self, self: _) -> T;
}

pub trait TryFrom<T> {
    fn try_from(&self, value: T) -> Result<Self, ConvertError>;
}

pub trait Displayable {
    fn to_string(&self, self: _) -> String;
}

pub struct ConvertError {
    pub message: String,
}

pub fn convert_error(message: String) -> ConvertError {
    ConvertError { message: message }
}

pub struct Celsius {
    pub degrees: f64,
}

pub struct Fahrenheit {
    pub degrees: f64,
}

impl From for Fahrenheit {
    fn from(&self, value: Celsius) -> Fahrenheit {
        Fahrenheit { degrees: ((value.degrees * 1.8_f64) + 32.0_f64) }
    }
}
