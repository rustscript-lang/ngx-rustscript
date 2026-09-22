//! Shared NGINX phase script slots.
//!
//! Request-phase VM execution lives in `access` / `request_state`.

use std::fmt;
use std::sync::Arc;

use vm::{BoundHostProgram, Program, Vm};

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

/// One compiled program together with its immutable production host binding.
///
/// The binding is prepared when configuration records the program. Runtime
/// callers only instantiate fresh VM-local state from `hosts` and then install
/// mutable request, policy, bridge, or timer state.
#[derive(Clone)]
pub struct PreparedScript {
    program: Arc<Program>,
    hosts: Arc<BoundHostProgram>,
    metadata: ScriptExecutionMetadata,
    ngx_catalog_abi: vm::HostApiFingerprint,
    standard_catalog_abi: vm::HostApiFingerprint,
}

impl fmt::Debug for PreparedScript {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedScript")
            .field("program", &(Arc::as_ptr(&self.program) as usize))
            .field("metadata", &self.metadata)
            .field("ngx_catalog_abi", &self.ngx_catalog_abi)
            .field("standard_catalog_abi", &self.standard_catalog_abi)
            .finish_non_exhaustive()
    }
}

impl PreparedScript {
    /// Prepare the immutable host dispatch exactly once for this record.
    pub fn prepare(
        program: Arc<Program>,
        metadata: ScriptExecutionMetadata,
    ) -> Result<Arc<Self>, String> {
        let hosts = crate::host::prepare_bound_program(Arc::clone(&program))?;
        Ok(Arc::new(Self {
            program,
            hosts,
            metadata,
            ngx_catalog_abi: crate::host::ngx_host_catalog_abi(),
            standard_catalog_abi: vm::standard_host_catalog_fingerprint(),
        }))
    }

    pub fn program(&self) -> &Arc<Program> {
        &self.program
    }

    pub fn hosts(&self) -> &Arc<BoundHostProgram> {
        &self.hosts
    }

    pub fn metadata(&self) -> ScriptExecutionMetadata {
        self.metadata
    }

    pub fn ngx_catalog_abi(&self) -> vm::HostApiFingerprint {
        self.ngx_catalog_abi
    }

    pub fn standard_catalog_abi(&self) -> vm::HostApiFingerprint {
        self.standard_catalog_abi
    }

    pub fn new_vm(&self) -> Result<Vm, String> {
        if self.ngx_catalog_abi != crate::host::ngx_host_catalog_abi() {
            return Err("prepared ngx host catalog ABI is stale".to_string());
        }
        if self.standard_catalog_abi != vm::standard_host_catalog_fingerprint() {
            return Err("prepared standard host catalog ABI is stale".to_string());
        }
        crate::host::instantiate_bound_vm(Arc::clone(&self.hosts))
    }
}

/// One compiled script attached to a location for a single HTTP phase.
#[derive(Clone, Debug, Default)]
pub struct ScriptLocationConf {
    program: Option<Arc<Program>>,
    prepared: Option<Arc<PreparedScript>>,
    metadata: ScriptExecutionMetadata,
}

/// Second `set_program` on the same slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplicateSlot;

/// Failure while recording a standalone phase slot.
#[derive(Debug)]
pub enum ScriptSetError {
    Duplicate,
    Prepare(String),
}

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

    pub fn prepared(&self) -> Option<&Arc<PreparedScript>> {
        self.prepared.as_ref()
    }

    pub fn set_program(&mut self, program: Arc<Program>) -> Result<(), DuplicateSlot> {
        self.set_program_with_metadata(program, ScriptExecutionMetadata::default())
    }

    pub fn set_program_with_metadata(
        &mut self,
        program: Arc<Program>,
        metadata: ScriptExecutionMetadata,
    ) -> Result<(), DuplicateSlot> {
        self.try_set_program_with_metadata(program, metadata)
            .map_err(|_| DuplicateSlot)
    }

    pub fn try_set_program_with_metadata(
        &mut self,
        program: Arc<Program>,
        metadata: ScriptExecutionMetadata,
    ) -> Result<(), ScriptSetError> {
        if self.program.is_some() {
            return Err(ScriptSetError::Duplicate);
        }
        let prepared = PreparedScript::prepare(Arc::clone(&program), metadata)
            .map_err(ScriptSetError::Prepare)?;
        self.set_prepared_program(program, metadata, prepared)
            .map_err(|DuplicateSlot| ScriptSetError::Duplicate)
    }

    pub(crate) fn set_prepared_program(
        &mut self,
        program: Arc<Program>,
        metadata: ScriptExecutionMetadata,
        prepared: Arc<PreparedScript>,
    ) -> Result<(), DuplicateSlot> {
        if self.program.is_some() {
            return Err(DuplicateSlot);
        }
        self.program = Some(program);
        self.prepared = Some(prepared);
        self.metadata = metadata;
        Ok(())
    }

    pub fn merge_from(&mut self, prev: &Self) {
        if self.program.is_none() {
            self.program.clone_from(&prev.program);
            self.prepared.clone_from(&prev.prepared);
            self.metadata = prev.metadata;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn prepared_program_instantiates_many_vms_without_request_bind_fallback() {
        let before = crate::host::binding_metrics_snapshot();
        let program = Arc::new(Program::new(Vec::new(), vec![vm::OpCode::Ret as u8]));
        let prepared = PreparedScript::prepare(program, ScriptExecutionMetadata::default())
            .expect("prepare empty program");

        for _ in 0..10_000 {
            prepared.new_vm().expect("instantiate bound VM");
        }

        let metrics = crate::host::binding_metrics_snapshot();
        assert_eq!(metrics.prepared_programs - before.prepared_programs, 1);
        assert_eq!(
            metrics.bound_vm_instances - before.bound_vm_instances,
            10_000
        );
        assert_eq!(metrics.full_bind_fallbacks - before.full_bind_fallbacks, 0);
    }
}
