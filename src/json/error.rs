//    Original file from: https://github.com/mre/hyperjson
//
//    Permission is hereby granted, free of charge, to any
//    person obtaining a copy of this software and associated
//    documentation files (the "Software"), to deal in the
//    Software without restriction, including without
//    limitation the rights to use, copy, modify, merge,
//    publish, distribute, sublicense, and/or sell copies of
//    the Software, and to permit persons to whom the Software
//    is furnished to do so, subject to the following
//    conditions:
//
//    The above copyright notice and this permission notice
//    shall be included in all copies or substantial portions
//    of the Software.
//
//    THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
//    ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
//    TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
//    PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
//    SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
//    CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
//    OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
//    IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//    DEALINGS IN THE SOFTWARE.

use failure::Fail;
use pyo3::exceptions::PyTypeError;
use pyo3::{import_exception, PyErr, PyObject};

#[derive(Debug, Fail)]
pub enum HyperJsonError {
    #[fail(display = "Conversion error: {}", error)]
    InvalidConversion { error: serde_json::Error },
    #[fail(display = "Python Runtime exception: {}", error)]
    PyErr { error: String },
    #[fail(display = "Dictionary key is not a string: {:?}", obj)]
    DictKeyNotString { obj: PyObject },
    #[fail(display = "Invalid float: {}", x)]
    InvalidFloat { x: String },
    #[fail(display = "Invalid type: {}, Error: {}", t, e)]
    InvalidCast { t: String, e: String },
    // NoneError doesn't have an impl for `Display`
    // See https://github.com/rust-lang-nursery/failure/issues/61
    // See https://github.com/rust-lang/rust/issues/42327#issuecomment-378324282
    // #[fail(display = "Error: {}", s)]
    // NoneError { s: String },
    #[fail(display = "Utf8 error: {}", error)]
    Utf8Error { error: std::string::FromUtf8Error },
}

impl From<serde_json::Error> for HyperJsonError {
    fn from(error: serde_json::Error) -> HyperJsonError {
        HyperJsonError::InvalidConversion { error }
    }
}

impl From<HyperJsonError> for PyErr {
    fn from(h: HyperJsonError) -> PyErr {
        match h {
            HyperJsonError::InvalidConversion { error } => {
                PyErr::new::<PyTypeError, _>(format!("{}", error))
            }
            // TODO
            HyperJsonError::PyErr { error: _error } => PyErr::new::<PyTypeError, _>("PyErr"),
            HyperJsonError::InvalidCast { t: _t, e: _e } => {
                PyErr::new::<PyTypeError, _>("InvalidCast")
            }
            _ => PyErr::new::<PyTypeError, _>("Unknown reason"),
        }
    }
}

impl From<PyErr> for HyperJsonError {
    fn from(error: PyErr) -> HyperJsonError {
        // TODO: This should probably just have the underlying PyErr as an argument,
        // but this type is not `Sync`, so we just use the debug representation for now.
        HyperJsonError::PyErr {
            error: format!("{:?}", error),
        }
    }
}

import_exception!(json, JSONDecodeError);
