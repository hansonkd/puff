fn main() {
    // Forward PyO3 Python configuration to our crate so we can use
    // #[cfg(Py_GIL_DISABLED)] in our code
    pyo3_build_config::use_pyo3_cfgs();
}
