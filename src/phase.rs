//! Shared NGINX phase script slots.
//!
//! Request-phase VM execution lives in `access` / `request_state`.

use std::sync::Arc;

use vm::Program;

/// Which compiled script slot a request is executing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseSlot {
    Access,
    Content,
}

impl PhaseSlot {
    pub fn directive(self) -> &'static str {
        match self {
            Self::Access => "access_by_rss_file",
            Self::Content => "content_by_rss_file",
        }
    }

    pub fn phase(self) -> crate::host::NgxPhase {
        match self {
            Self::Access => crate::host::NgxPhase::Access,
            Self::Content => crate::host::NgxPhase::Content,
        }
    }
}

/// One compiled script attached to a location for a single HTTP phase.
#[derive(Clone, Debug, Default)]
pub struct ScriptLocationConf {
    program: Option<Arc<Program>>,
}

/// Second `set_program` on the same slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplicateSlot;

impl ScriptLocationConf {
    pub fn is_set(&self) -> bool {
        self.program.is_some()
    }

    pub fn program(&self) -> Option<&Arc<Program>> {
        self.program.as_ref()
    }

    pub fn set_program(&mut self, program: Arc<Program>) -> Result<(), DuplicateSlot> {
        if self.program.is_some() {
            return Err(DuplicateSlot);
        }
        self.program = Some(program);
        Ok(())
    }

    pub fn merge_from(&mut self, prev: &Self) {
        if self.program.is_none() {
            self.program.clone_from(&prev.program);
        }
    }
}
