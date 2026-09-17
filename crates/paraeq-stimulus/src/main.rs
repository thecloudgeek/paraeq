//! The one binary target. A shim: everything it does lives in the library, so
//! `tests/` can drive the whole program with no process and no device.

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(paraeq_stimulus::run(std::env::args_os()))
}
