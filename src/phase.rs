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

/// Cached execution routing discovered once when the script is compiled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScriptExecutionMetadata {
    pub requires_request_state: bool,
    pub reuses_request_vm: bool,
    pub uses_sqlite: bool,
}

/// One compiled script attached to a location for a single HTTP phase.
#[derive(Clone, Debug, Default)]
pub struct ScriptLocationConf {
    program: Option<Arc<Program>>,
    metadata: ScriptExecutionMetadata,
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

    pub fn metadata(&self) -> ScriptExecutionMetadata {
        self.metadata
    }

    pub fn set_program(&mut self, program: Arc<Program>) -> Result<(), DuplicateSlot> {
        self.set_program_with_metadata(program, ScriptExecutionMetadata::default())
    }

    pub fn set_program_with_metadata(
        &mut self,
        program: Arc<Program>,
        metadata: ScriptExecutionMetadata,
    ) -> Result<(), DuplicateSlot> {
        if self.program.is_some() {
            return Err(DuplicateSlot);
        }
        self.program = Some(program);
        self.metadata = metadata;
        Ok(())
    }

    pub fn merge_from(&mut self, prev: &Self) {
        if self.program.is_none() {
            self.program.clone_from(&prev.program);
            self.metadata = prev.metadata;
        }
    }
}
