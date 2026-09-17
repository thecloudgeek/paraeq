//! The SIGTERM rung.
//!
//! **Everything in this crate's test suite that touches the abort flag lives in
//! THIS file**, and the file has exactly one test that touches it. The flag is
//! a process-global `static` with no way to clear it — deliberately, because a
//! reset hook would be a second way to un-arm a safety abort — so it can be
//! observed to transition exactly once per process, and two tests racing for
//! that one transition would be two tests asserting whichever ran first.

use paraeq_stimulus::signals;

#[test]
fn sigterm_arms_the_same_abort_flag_stdin_does() {
    // ONE cell, asserted two ways, because a process-global static can only be
    // watched to transition once.
    //
    // Why this rung exists at all: the parent sends SIGTERM before it ever
    // sends SIGKILL, and that only helps if this process HANDLES it. SIGTERM's
    // default disposition terminates without unwinding, so the render
    // aggregate's Drop never runs and a private aggregate is left wrapping the
    // user's output device. With the handler, SIGTERM ramps.
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/signals.rs"))
        .expect("signals.rs is readable");
    assert_eq!(
        src.matches("static ABORT").count(),
        1,
        "there must be exactly one abort cell: the stdin path and the signal handler converge \
         on it, which is what makes SIGTERM ramp rather than click"
    );
    assert!(
        !src.contains("store(false"),
        "nothing may clear the abort flag: a reset is a second way to un-arm a safety abort"
    );

    assert!(!signals::aborting(), "the flag starts clear");

    signals::on_sigterm(libc::SIGTERM);
    assert!(signals::aborting(), "SIGTERM arms the abort");

    signals::request_abort();
    assert!(
        signals::aborting(),
        "and the stdin path writes the same cell — it cannot clear what SIGTERM set"
    );
}

#[test]
fn arming_the_handler_is_idempotent_and_touches_nothing_else() {
    // `arm` is called once from `run`, before any other thread exists. Calling
    // it twice must not be a second kind of event — the handler is replaced by
    // an identical one.
    signals::arm();
    signals::arm();
}

#[test]
fn the_handler_body_is_one_store() {
    // The handler runs on a signal frame, where allocating, locking, logging
    // and formatting are all unsafe: a handler that took a lock the interrupted
    // thread already held would deadlock the process with the device still
    // open. A store to an AtomicBool is async-signal-safe; nothing else here
    // is, so nothing else belongs here.
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/signals.rs"))
        .expect("signals.rs is readable");
    let body = src
        .split_once("pub extern \"C\" fn on_sigterm")
        .expect("the handler is declared")
        .1
        .split_once('{')
        .expect("the handler has a body")
        .1
        .split_once('}')
        .expect("the handler's body ends")
        .0;
    let statements: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .collect();
    assert_eq!(
        statements,
        vec!["ABORT.store(true, Ordering::SeqCst);"],
        "the handler grew a second statement"
    );
}

#[test]
#[ignore = "requires audio hardware"]
fn a_sigtermed_child_ramps_and_exits_zero() {
    // The falsifier for the whole rung. With no handler installed the child
    // dies OF SIGTERM — a signal death, exit status 143 to a shell and a
    // `signalled` exit here — and its render aggregate is left wrapping the
    // user's output device. With the handler, SIGTERM writes the same flag
    // `abort` does, the stimulus fades over 5 ms, the device is destroyed, and
    // the process exits 0.
    //
    // Hardware-gated because it plays: a silent file, but through a real
    // device.
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    let out = paraeq_coreaudio::properties::default_output_device().expect("a default output");
    let uid = paraeq_coreaudio::properties::device_uid(out).expect("its UID");
    let rate = paraeq_coreaudio::properties::nominal_sample_rate(out).expect("its rate") as u32;

    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("sigterm");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let wav_path = dir.join("silence.wav");
    let mut writer = hound::WavWriter::create(
        &wav_path,
        hound::WavSpec {
            bits_per_sample: 32,
            channels: 1,
            sample_format: hound::SampleFormat::Float,
            sample_rate: rate,
        },
    )
    .expect("a writable WAV");
    for _ in 0..(rate * 5) {
        writer.write_sample(0.0f32).expect("write");
    }
    writer.finalize().expect("finalize");

    let mut child = Command::new(env!("CARGO_BIN_EXE_paraeq-stimulus"))
        .args([
            "--wav",
            wav_path.to_str().expect("a utf-8 scratch path"),
            "--device-uid",
            &uid,
            "--channel",
            "both",
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the helper binary is built beside this test");

    let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut line = String::new();
    stdout.read_line(&mut line).expect("the ready line");
    assert!(line.contains(r#""event":"ready""#), "{line}");

    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(b"play\n")
        .expect("the parent commits");

    std::thread::sleep(std::time::Duration::from_millis(500));
    // SAFETY: `pid` is a child this process spawned and has not yet reaped, and
    // SIGTERM is a valid signal number on every supported target.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };

    let status = child.wait().expect("the child exits");
    assert_eq!(
        status.code(),
        Some(0),
        "SIGTERM must be HANDLED: a signal death means no handler, which means the render \
         aggregate's Drop never ran"
    );

    let mut saw_aborted = false;
    for line in stdout.lines() {
        let line = line.expect("a readable stdout line");
        if line.contains(r#""event":"aborted""#) {
            saw_aborted = true;
        }
    }
    assert!(saw_aborted, "the child reported a clean abort, not a stop");

    assert_eq!(
        paraeq_coreaudio::properties::translate_uid_to_device(&format!(
            "com.paraeq.render.{}",
            child.id()
        ))
        .expect("the lookup itself succeeds"),
        0,
        "the child's render aggregate is gone"
    );
}
