use moodeng_core::Session;
use std::collections::HashMap;

/// Per-connection session state.
pub struct ConnectionSession {
    pub session: Session,
    pub prepared: HashMap<String, String>,
    pub portal_params: Vec<Option<String>>,
    pub last_statement: Option<String>,
    pub in_error: bool,
}

impl ConnectionSession {
    pub fn new() -> Self {
        Self {
            session: Session::new(),
            prepared: HashMap::new(),
            portal_params: Vec::new(),
            last_statement: None,
            in_error: false,
        }
    }

    pub fn ready_status(&self) -> u8 {
        if self.in_error {
            b'E'
        } else if self.session.transaction.is_active() {
            b'T'
        } else {
            b'I'
        }
    }
}

impl Default for ConnectionSession {
    fn default() -> Self {
        Self::new()
    }
}
