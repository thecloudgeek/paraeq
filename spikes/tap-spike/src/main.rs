mod biquad;
mod ca;

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectID,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};

/// Shared with the realtime callback. No locks; atomics + preallocated filters only.
struct EqState {
    filters: [biquad::Df2t; 2],
    bypass: bool,
    callbacks: AtomicU64,
    /// f32 bits of the max |sample| seen on input (AtomicU32 as f32 bits).
    peak_in_bits: AtomicU32,
    zero_blocks: AtomicU64,
    nonzero_blocks: AtomicU64,
    /// (output sample time - input sample time) of the latest callback.
    sample_time_delta: AtomicU64, // f64 bits
}

/// Copy an AudioBufferList's buffers into per-channel access without allocating:
/// we iterate buffers in place. Layout is probed at runtime (may be one
/// interleaved stereo buffer, or two mono buffers — DGR Labs gotcha).
unsafe fn buffers_of(list: NonNull<AudioBufferList>) -> &'static mut [objc2_core_audio_types::AudioBuffer] {
    let l = list.as_ptr();
    let n = (*l).mNumberBuffers as usize;
    std::slice::from_raw_parts_mut((*l).mBuffers.as_mut_ptr(), n)
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    input_time: NonNull<AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let state = &mut *(client as *mut EqState);
    state.callbacks.fetch_add(1, Ordering::Relaxed);
    let dt = (*output_time.as_ptr()).mSampleTime - (*input_time.as_ptr()).mSampleTime;
    state.sample_time_delta.store(dt.to_bits(), Ordering::Relaxed);

    let in_bufs = buffers_of(input);
    let out_bufs = buffers_of(output);

    // Zero all output buffers first (whatever their layout).
    for ob in out_bufs.iter_mut() {
        let n = ob.mDataByteSize as usize / 4;
        let data = std::slice::from_raw_parts_mut(ob.mData as *mut f32, n);
        data.fill(0.0);
    }

    let mut peak = f32::from_bits(state.peak_in_bits.load(Ordering::Relaxed));
    let mut all_zero = true;

    // Process channel-by-channel. `ch` is the logical stereo channel index.
    // Input layout cases: 1 buffer with mNumberChannels==2 (interleaved) or
    // 2 buffers with mNumberChannels==1 (deinterleaved) or mono.
    let mut logical_ch = 0usize;
    for ib in in_bufs.iter() {
        let ch_in_buf = ib.mNumberChannels.max(1) as usize;
        let n = ib.mDataByteSize as usize / 4;
        let data = std::slice::from_raw_parts(ib.mData as *const f32, n);
        for interleave in 0..ch_in_buf {
            let ch = (logical_ch + interleave).min(1);
            let filt = &mut state.filters[ch];
            let mut i = interleave;
            while i < n {
                let x = data[i];
                if x != 0.0 {
                    all_zero = false;
                }
                let ax = x.abs();
                if ax > peak {
                    peak = ax;
                }
                let y = if state.bypass { x } else { filt.process(x) };
                // Write into the matching output slot, probing output layout.
                write_sample(out_bufs, ch, i / ch_in_buf, ch_in_buf, y);
                i += ch_in_buf;
            }
        }
        logical_ch += ch_in_buf;
    }

    state.peak_in_bits.store(peak.to_bits(), Ordering::Relaxed);
    if all_zero {
        state.zero_blocks.fetch_add(1, Ordering::Relaxed);
    } else {
        state.nonzero_blocks.fetch_add(1, Ordering::Relaxed);
    }
    0
}

/// Write sample y for logical channel `ch`, frame `frame`, into whatever
/// layout the output buffers use.
unsafe fn write_sample(
    out_bufs: &mut [objc2_core_audio_types::AudioBuffer],
    ch: usize,
    frame: usize,
    _in_ch_per_buf: usize,
    y: f32,
) {
    if out_bufs.len() == 1 {
        let ob = &mut out_bufs[0];
        let ch_n = ob.mNumberChannels.max(1) as usize;
        let n = ob.mDataByteSize as usize / 4;
        let idx = frame * ch_n + ch.min(ch_n - 1);
        if idx < n {
            let data = std::slice::from_raw_parts_mut(ob.mData as *mut f32, n);
            data[idx] = y;
        }
    } else if ch < out_bufs.len() {
        let ob = &mut out_bufs[ch];
        let n = ob.mDataByteSize as usize / 4;
        if frame < n {
            let data = std::slice::from_raw_parts_mut(ob.mData as *mut f32, n);
            data[frame] = y;
        }
    }
}

struct Cli {
    bypass: bool,
    fc: f64,
    gain: f64,
    q: f64,
    secs: u64,
}

fn parse_cli(args: &[String]) -> Cli {
    let mut c = Cli { bypass: false, fc: 80.0, gain: 6.0, q: 1.0, secs: 0 };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bypass" => c.bypass = true,
            "--fc" => { i += 1; c.fc = args[i].parse().unwrap(); }
            "--gain" => { i += 1; c.gain = args[i].parse().unwrap(); }
            "--q" => { i += 1; c.q = args[i].parse().unwrap(); }
            "--secs" => { i += 1; c.secs = args[i].parse().unwrap(); }
            other => eprintln!("ignoring arg: {other}"),
        }
        i += 1;
    }
    c
}

/// Run a teardown call and surface a failure instead of discarding it —
/// teardown OSStatus errors must be visible even though we can't recover.
fn teardown_step(status: i32, ctx: &str) {
    ca::check(status, ctx).unwrap_or_else(|e| eprintln!("teardown: {e}"));
}

fn run(cli: Cli) -> Result<(), String> {
    // 1-2. devices + tap
    let dev = ca::default_output_device()?;
    let uid = ca::device_uid(dev)?;
    let rate = ca::nominal_sample_rate(dev)?;
    println!("default output: id={dev} uid={uid} rate={rate}");

    let own = ca::translate_pid(std::process::id() as i32)?;
    let excluded = if own != 0 { vec![own] } else { vec![] };
    if excluded.is_empty() {
        println!("WARN: own process not in HAL registry yet — no self-exclusion (watch for feedback)");
    }
    let (tap, desc) = ca::create_tap(&excluded)?;
    let fmt = ca::tap_format(tap)?;
    println!("tap: id={tap}, {} Hz, {} ch", fmt.mSampleRate, fmt.mChannelsPerFrame);

    // 3. aggregate (tap must be in the creation dict)
    let tap_uuid = unsafe { desc.UUID().UUIDString() };
    let agg = ca::create_aggregate(&uid, &tap_uuid)?;
    println!("aggregate: id={agg}");

    // 4. EQ state + IOProc
    let coeffs = biquad::peaking(rate, cli.fc, cli.gain, cli.q);
    let mut state = Box::new(EqState {
        filters: [biquad::Df2t::new(coeffs), biquad::Df2t::new(coeffs)],
        bypass: cli.bypass,
        callbacks: AtomicU64::new(0),
        peak_in_bits: AtomicU32::new(0),
        zero_blocks: AtomicU64::new(0),
        nonzero_blocks: AtomicU64::new(0),
        sample_time_delta: AtomicU64::new(0),
    });
    println!(
        "EQ: peaking fc={} gain={} q={} bypass={}",
        cli.fc, cli.gain, cli.q, cli.bypass
    );

    let mut proc_id: AudioDeviceIOProcID = None;
    let status = unsafe {
        AudioDeviceCreateIOProcID(
            agg,
            Some(io_proc),
            (&mut *state as *mut EqState).cast::<c_void>(),
            NonNull::from(&mut proc_id),
        )
    };
    ca::check(status, "AudioDeviceCreateIOProcID")?;
    ca::check(unsafe { AudioDeviceStart(agg, proc_id) }, "AudioDeviceStart")?;
    println!("RUNNING — play music. Ctrl-C to stop. (--secs {} means {})",
        cli.secs, if cli.secs == 0 { "until Ctrl-C".to_string() } else { format!("{}s", cli.secs) });

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .map_err(|e| e.to_string())?;
    }

    let started = std::time::Instant::now();
    let mut last_cb = 0u64;
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let cb = state.callbacks.load(Ordering::Relaxed);
        let dt = f64::from_bits(state.sample_time_delta.load(Ordering::Relaxed));
        println!(
            "cb/s={:4}  peak_in={:.3}  zero_blocks={}  nonzero={}  out-in sample delta={} ({:.1} ms)",
            cb - last_cb,
            f32::from_bits(state.peak_in_bits.load(Ordering::Relaxed)),
            state.zero_blocks.load(Ordering::Relaxed),
            state.nonzero_blocks.load(Ordering::Relaxed),
            dt,
            1000.0 * dt / rate,
        );
        last_cb = cb;
        state.peak_in_bits.store(0, Ordering::Relaxed);
        if cli.secs > 0 && started.elapsed().as_secs() >= cli.secs {
            break;
        }
    }

    // Teardown — ORDER IS THE INVARIANT: stop → destroy proc → destroy agg → destroy tap.
    // Each step's OSStatus is surfaced (not discarded) so teardown failures are visible.
    println!("tearing down…");
    unsafe {
        teardown_step(AudioDeviceStop(agg, proc_id), "AudioDeviceStop");
        teardown_step(AudioDeviceDestroyIOProcID(agg, proc_id), "AudioDeviceDestroyIOProcID");
        teardown_step(AudioHardwareDestroyAggregateDevice(agg), "AudioHardwareDestroyAggregateDevice");
        teardown_step(AudioHardwareDestroyProcessTap(tap), "AudioHardwareDestroyProcessTap");
    }
    println!("done — system audio should be back to normal.");
    Ok(())
}

fn probe() -> Result<(), String> {
    let dev = ca::default_output_device()?;
    let uid = ca::device_uid(dev)?;
    let rate = ca::nominal_sample_rate(dev)?;
    println!("default output: id={dev} uid={uid} rate={rate}");
    let own = ca::translate_pid(std::process::id() as i32)?;
    println!("own process object: {own}");
    let excluded = if own != 0 { vec![own] } else { vec![] };
    let (tap, _desc) = ca::create_tap(&excluded)?;
    let f = ca::tap_format(tap)?;
    println!(
        "tap created: id={tap} format: {} Hz, {} ch, {} bits, flags={:#x}",
        f.mSampleRate, f.mChannelsPerFrame, f.mBitsPerChannel, f.mFormatFlags
    );
    teardown_step(
        unsafe { objc2_core_audio::AudioHardwareDestroyProcessTap(tap) },
        "AudioHardwareDestroyProcessTap",
    );
    println!("tap destroyed. probe OK");
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--probe") => probe(),
        Some("run") => run(parse_cli(&args[1..])),
        _ => {
            eprintln!("usage: tap-spike --probe | run [--bypass] [--fc HZ] [--gain DB] [--q Q] [--secs N]");
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("FATAL: {e}");
        std::process::exit(1);
    }
}
