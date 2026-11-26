//! The stream's half of the retry policy: which failed reads are worth
//! another attempt.
//!
//! A stream that fails before it has shown anything costs nothing to ask
//! for again. No delta reached the caller, so a second attempt cannot
//! repeat itself. From the first delta on the opposite holds, and a cut
//! keeps its partial (`complete: false`). The decoders wrap only the first
//! kind in [`RetryableStream`]; the client's retry loop reads the type and
//! spends the same budget the response head does.

use std::fmt;

/// A stream that failed before it delivered anything, in a way another
/// attempt could mend: the transport broke under the read, or the body
/// ended before its sentinel having assembled nothing. Malformed chunks
/// and provider error events stay unwrapped: the same request would get
/// the same answer.
///
/// The wrapper adds no words. What it says is the whole chain of the
/// failure inside it, which is also what the caller reads once the retry
/// budget is spent.
#[derive(Debug)]
pub(in crate::llm) struct RetryableStream(pub(in crate::llm) anyhow::Error);

impl fmt::Display for RetryableStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for RetryableStream {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wrapper is found by type, and says what is inside it.
    #[test]
    fn a_wrapped_error_reads_as_its_own_chain() {
        let error: anyhow::Error =
            RetryableStream(anyhow::anyhow!("cut").context("reading")).into();
        assert!(error.downcast_ref::<RetryableStream>().is_some());
        assert_eq!(error.to_string(), "reading: cut");
    }
}
