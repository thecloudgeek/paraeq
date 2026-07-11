//! Typed OSStatus errors with fourcc decode (e.g. '!obj', 'who?').
//! Ported from the validated tap spike (spikes/tap-spike/src/ca.rs:35-45).

/// A CoreAudio call failed: the OSStatus, its fourcc rendering, and what we
/// were doing at the time.
#[derive(Debug, thiserror::Error)]
#[error("{ctx}: OSStatus {status} ('{fourcc}')")]
pub struct CaError {
    pub status: i32,
    pub fourcc: String,
    pub ctx: String,
}

/// Decode an OSStatus into Ok/Err with the fourcc shown (non-graphic bytes
/// become '?', e.g. -1 renders as "????").
pub fn check(status: i32, ctx: &str) -> Result<(), CaError> {
    if status == 0 {
        return Ok(());
    }
    let fourcc: String = status
        .to_be_bytes()
        .iter()
        .map(|&c| if c.is_ascii_graphic() { c as char } else { '?' })
        .collect();
    Err(CaError {
        status,
        fourcc,
        ctx: ctx.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_status_is_ok() {
        assert!(check(0, "anything").is_ok());
    }

    #[test]
    fn fourcc_decodes_ascii_graphic_bytes() {
        // 0x216f626a is kAudioHardwareBadObjectError ('!obj').
        let err = check(0x216f_626a, "create tap").unwrap_err();
        assert_eq!(err.status, 0x216f_626a);
        assert_eq!(err.fourcc, "!obj");
        assert_eq!(err.ctx, "create tap");
        assert_eq!(
            err.to_string(),
            format!("create tap: OSStatus {} ('!obj')", 0x216f_626au32)
        );
    }

    #[test]
    fn non_graphic_bytes_pad_with_question_marks() {
        // Small negative ints have no printable fourcc: every byte -> '?'.
        assert_eq!(check(-1, "ctx").unwrap_err().fourcc, "????");
        assert_eq!(check(-50, "ctx").unwrap_err().fourcc, "????");
    }
}
