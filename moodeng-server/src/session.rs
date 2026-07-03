use moodeng_core::Session;

/// Per-connection session state (transaction + future session vars).
pub struct ConnectionSession {
    pub session: Session,
}

impl ConnectionSession {
    pub fn new() -> Self {
        Self {
            session: Session::new(),
        }
    }
}

impl Default for ConnectionSession {
    fn default() -> Self {
        Self::new()
    }
}
