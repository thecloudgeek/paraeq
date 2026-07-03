mod biquad;
mod ca;

fn probe() -> Result<(), String> {
    let dev = ca::default_output_device()?;
    let uid = ca::device_uid(dev)?;
    let rate = ca::nominal_sample_rate(dev)?;
    println!("default output: id={dev} uid={uid} rate={rate}");

    let own = ca::translate_pid(std::process::id() as i32)?;
    println!("own process object: {own} (0 = not registered with HAL yet; exclusion list will be empty)");

    let excluded = if own != 0 { vec![own] } else { vec![] };
    let (tap, _desc) = ca::create_tap(&excluded)?;
    let f = ca::tap_format(tap)?;
    println!(
        "tap created: id={tap} format: {} Hz, {} ch, {} bits, flags={:#x}",
        f.mSampleRate, f.mChannelsPerFrame, f.mBitsPerChannel, f.mFormatFlags
    );
    unsafe { objc2_core_audio::AudioHardwareDestroyProcessTap(tap) };
    println!("tap destroyed. probe OK");
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("--probe") => probe(),
        _ => {
            eprintln!("usage: tap-spike --probe   (run loop lands in Task 3)");
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("FATAL: {e}");
        std::process::exit(1);
    }
}
