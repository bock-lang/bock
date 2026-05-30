#![allow(unused_variables, unused_imports, unused_parens, dead_code, non_upper_case_globals)]

pub trait Error {
    fn message(&self, self: _) -> String;
}

pub struct SimpleError {
    pub message: String,
}

impl Error for SimpleError {
    fn message(&self, self: _) -> String {
        self.message
    }
}

pub fn error(message: String) -> SimpleError {
    SimpleError { message: message }
}
