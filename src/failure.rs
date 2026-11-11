// ---------------------------------------------------------------------------
// Failures that carry a process exit code
// ---------------------------------------------------------------------------

/// A failed command: the message to print and the exit code to return.
/// The codes are the ones xdg-open documents.
#[derive(Debug)]
pub struct Failure {
    pub code: i32,
    pub message: String,
}

impl Failure {
    pub const USAGE: i32 = 1;
    pub const MISSING: i32 = 2;
    pub const NO_ASSOCIATION: i32 = 3;
    pub const LAUNCH: i32 = 4;
    pub const READ: i32 = 5;
    pub const WRITE: i32 = 6;

    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Failure {
            code,
            message: message.into(),
        }
    }
}
