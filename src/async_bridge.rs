//! Generic pull-async host bridge.
//!
//! [`NgxAsyncBridge`] stores `HostOpId -> HostFuture`, rejects duplicates,
//! polls under the worker Tokio handle, and removes the future on Ready or
//! cancel. It is host-agnostic: `ngx::sleep` and any other macro async host
//! share the same map.

use std::collections::HashMap;
use std::task::{Context, Poll};

use tokio::runtime::Handle;
use vm::operation::OperationCancelReason;
use vm::{CallReturn, HostAsyncBridge, HostFuture, HostOpId, VmError, VmResult};

/// Worker-owned async host future table.
pub struct NgxAsyncBridge {
    runtime: Handle,
    futures: HashMap<HostOpId, HostFuture>,
}

impl NgxAsyncBridge {
    pub fn new(runtime: Handle) -> Self {
        Self {
            runtime,
            futures: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.futures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.futures.is_empty()
    }

    pub fn contains(&self, op_id: HostOpId) -> bool {
        self.futures.contains_key(&op_id)
    }
}

impl HostAsyncBridge for NgxAsyncBridge {
    fn submit_op(&mut self, op_id: HostOpId, future: HostFuture) -> VmResult<()> {
        if self.futures.contains_key(&op_id) {
            return Err(VmError::HostError(format!(
                "duplicate host op {op_id} rejected"
            )));
        }
        self.futures.insert(op_id, future);
        Ok(())
    }

    fn poll_op(&mut self, op_id: HostOpId, _cx: &mut Context<'_>) -> Poll<VmResult<CallReturn>> {
        Poll::Ready(Err(VmError::HostError(format!(
            "host op {op_id} is bridge-owned; poll_submitted_op is required"
        ))))
    }

    fn poll_submitted_op(
        &mut self,
        op_id: HostOpId,
        cx: &mut Context<'_>,
    ) -> Poll<VmResult<vm::HostFutureOutput>> {
        let Some(future) = self.futures.get_mut(&op_id) else {
            return Poll::Ready(Err(VmError::HostError(format!(
                "unknown submitted host op {op_id}"
            ))));
        };
        let _enter = self.runtime.enter();
        match future.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.futures.remove(&op_id);
                Poll::Ready(result)
            }
        }
    }

    fn cancel_op(&mut self, op_id: HostOpId) {
        self.futures.remove(&op_id);
    }

    fn request_cancel_op(
        &mut self,
        op_id: HostOpId,
        _reason: OperationCancelReason,
    ) -> VmResult<()> {
        self.futures.remove(&op_id);
        Ok(())
    }

    fn poll_cancel_op(&mut self, op_id: HostOpId, _cx: &mut Context<'_>) -> Poll<VmResult<()>> {
        self.futures.remove(&op_id);
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{self, Future};
    use std::task::Waker;
    use std::time::Duration;

    use vm::{CallReturn, HostFutureOutput, Vm, VmStatus};

    use crate::host::{bind_ngx_hosts, bind_test_hosts};
    use crate::notify::{NotifyBridge, RequestIdentity, eventfd_waker};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .expect("tokio runtime")
    }

    fn compile_with(source: &str, catalog: vm::HostApiCatalog) -> vm::Program {
        vm::compile_source_with_flavor_and_options(
            source,
            vm::SourceFlavor::RustScript,
            vm::CompileSourceFileOptions::default()
                .with_host_api_catalog(std::sync::Arc::new(catalog)),
        )
        .expect("compile")
        .program
    }

    fn sleep_catalog() -> vm::HostApiCatalog {
        crate::host::ngx_host_catalog()
    }

    fn test_catalog() -> vm::HostApiCatalog {
        let mut builder = vm::HostApiBuilder::new();
        builder.function(vm::HostFunctionSchema::with_return(
            "ngx::sleep",
            vec![vm::HostParamSchema::value("ms", vm::HostTypeSchema::Int)],
            vm::HostTypeSchema::Null,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            "ngx::probe",
            vec![vm::HostParamSchema::value("ms", vm::HostTypeSchema::Int)],
            vm::HostTypeSchema::Int,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            "ngx::now",
            vec![],
            vm::HostTypeSchema::Int,
        ));
        builder.function(vm::HostFunctionSchema::with_return(
            "ngx::fail",
            vec![],
            vm::HostTypeSchema::Int,
        ));
        builder.build().expect("test catalog")
    }

    fn bind_vm(program: vm::Program, handle: Handle, test_hosts: bool) -> Vm {
        let mut vm = Vm::new(program);
        if test_hosts {
            bind_test_hosts(&mut vm);
        } else {
            bind_ngx_hosts(&mut vm);
        }
        vm.set_async_bridge(Box::new(NgxAsyncBridge::new(handle)))
            .expect("async bridge");
        vm
    }

    #[test]
    fn waiting_initial_pending_ready_poll_resume() {
        let rt = runtime();
        let program = compile_with("use ngx;\nngx::sleep(5);\n", sleep_catalog());
        let mut vm = bind_vm(program, rt.handle().clone(), false);
        let status = vm.run().expect("run");
        let VmStatus::Waiting(_op_id) = status else {
            panic!("expected Waiting, got {status:?}");
        };

        let notify = NotifyBridge::new().unwrap();
        let identity = RequestIdentity::new(1, 1);
        let waker = eventfd_waker(notify.sender(), identity);
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(vm.poll_waiting_host_op(&mut cx), Poll::Pending));

        let fd = notify.raw_fd().expect("eventfd");
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut pfd, 1, 20) };
            if n > 0 && pfd.revents & libc::POLLIN != 0 {
                break;
            }
        }
        let ready = notify.harvest_and_take_ready().expect("harvest");
        assert_eq!(ready, vec![identity]);

        let noop = Waker::noop();
        let mut cx = Context::from_waker(noop);
        assert!(matches!(
            vm.poll_waiting_host_op(&mut cx),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(vm.resume().expect("resume"), VmStatus::Halted);
    }

    #[test]
    fn immediate_ready_async_host_resumes_without_wait() {
        let rt = runtime();
        let program = compile_with("use ngx;\nngx::now();\n", test_catalog());
        let mut vm = bind_vm(program, rt.handle().clone(), true);
        let status = vm.run().expect("run");
        let VmStatus::Waiting(_) = status else {
            panic!("expected Waiting, got {status:?}");
        };
        let noop = Waker::noop();
        let mut cx = Context::from_waker(noop);
        assert!(matches!(
            vm.poll_waiting_host_op(&mut cx),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(vm.resume().expect("resume"), VmStatus::Halted);
    }

    #[test]
    fn two_consecutive_async_calls() {
        let rt = runtime();
        let program = compile_with(
            "use ngx;\nngx::sleep(1);\nngx::sleep(1);\n",
            sleep_catalog(),
        );
        let mut vm = bind_vm(program, rt.handle().clone(), false);
        assert!(matches!(vm.run().expect("run"), VmStatus::Waiting(_)));

        let notify = NotifyBridge::new().unwrap();
        let first = RequestIdentity::new(1, 1);
        let waker = eventfd_waker(notify.sender(), first);
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(vm.poll_waiting_host_op(&mut cx), Poll::Pending));
        let fd = notify.raw_fd().expect("eventfd");
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut pfd, 1, 20) } > 0 {
                break;
            }
        }
        let _ = notify.harvest_and_take_ready();
        let noop = Waker::noop();
        let mut cx = Context::from_waker(noop);
        assert!(matches!(
            vm.poll_waiting_host_op(&mut cx),
            Poll::Ready(Ok(()))
        ));
        assert!(matches!(vm.resume().expect("resume"), VmStatus::Waiting(_)));

        let second = RequestIdentity::new(1, 1);
        let waker = eventfd_waker(notify.sender(), second);
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(vm.poll_waiting_host_op(&mut cx), Poll::Pending));
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut pfd, 1, 20) } > 0 {
                break;
            }
        }
        let _ = notify.harvest_and_take_ready();
        let mut cx = Context::from_waker(noop);
        assert!(matches!(
            vm.poll_waiting_host_op(&mut cx),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(vm.resume().expect("resume"), VmStatus::Halted);
    }

    #[test]
    fn second_macro_async_host_shares_the_bridge() {
        let rt = runtime();
        let program = compile_with("use ngx;\nngx::sleep(1);\nngx::probe(1);\n", test_catalog());
        let mut vm = bind_vm(program, rt.handle().clone(), true);
        assert!(matches!(vm.run().expect("run"), VmStatus::Waiting(_)));

        let notify = NotifyBridge::new().unwrap();
        let identity = RequestIdentity::new(3, 1);
        for _ in 0..2 {
            let waker = eventfd_waker(notify.sender(), identity);
            let mut cx = Context::from_waker(&waker);
            assert!(matches!(vm.poll_waiting_host_op(&mut cx), Poll::Pending));
            let fd = notify.raw_fd().expect("eventfd");
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(2) {
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                if unsafe { libc::poll(&mut pfd, 1, 20) } > 0 {
                    break;
                }
            }
            let _ = notify.harvest_and_take_ready();
            let noop = Waker::noop();
            let mut cx = Context::from_waker(noop);
            assert!(matches!(
                vm.poll_waiting_host_op(&mut cx),
                Poll::Ready(Ok(()))
            ));
            let status = vm.resume().expect("resume");
            if matches!(status, VmStatus::Halted) {
                return;
            }
            assert!(matches!(status, VmStatus::Waiting(_)));
        }
        panic!("probe did not complete");
    }

    #[test]
    fn duplicate_op_is_rejected() {
        let rt = runtime();
        let mut bridge = NgxAsyncBridge::new(rt.handle().clone());
        let fut: HostFuture =
            Box::pin(async { Ok(HostFutureOutput::returning(CallReturn::none())) });
        bridge.submit_op(7, fut).expect("first");
        let fut: HostFuture =
            Box::pin(async { Ok(HostFutureOutput::returning(CallReturn::none())) });
        let err = bridge.submit_op(7, fut).expect_err("duplicate");
        assert!(err.to_string().contains("duplicate host op 7"));
        assert_eq!(bridge.len(), 1);
    }

    #[test]
    fn ready_and_error_remove_the_future() {
        let rt = runtime();
        let mut bridge = NgxAsyncBridge::new(rt.handle().clone());
        let ok: HostFuture =
            Box::pin(async { Ok(HostFutureOutput::returning(CallReturn::none())) });
        bridge.submit_op(1, ok).unwrap();
        let noop = Waker::noop();
        let mut cx = Context::from_waker(noop);
        assert!(matches!(
            bridge.poll_submitted_op(1, &mut cx),
            Poll::Ready(Ok(_))
        ));
        assert!(!bridge.contains(1));

        let err_fut: HostFuture = Box::pin(async { Err(VmError::HostError("boom".into())) });
        bridge.submit_op(2, err_fut).unwrap();
        assert!(matches!(
            bridge.poll_submitted_op(2, &mut cx),
            Poll::Ready(Err(_))
        ));
        assert!(!bridge.contains(2));
    }

    #[test]
    fn cancel_and_reset_drop_futures() {
        let rt = runtime();
        let mut bridge = NgxAsyncBridge::new(rt.handle().clone());
        let pending: HostFuture = Box::pin(future::pending());
        bridge.submit_op(1, pending).unwrap();
        assert!(bridge.contains(1));
        assert_eq!(bridge.len(), 1);
        bridge.cancel_op(1);
        assert!(
            !bridge.contains(1),
            "cancel_op must drop the pending future"
        );
        assert_eq!(bridge.len(), 0);

        let pending: HostFuture = Box::pin(future::pending());
        bridge.submit_op(2, pending).unwrap();
        bridge
            .request_cancel_op(2, OperationCancelReason::Requested)
            .unwrap();
        assert!(
            !bridge.contains(2),
            "request_cancel_op must drop the pending future"
        );

        let mut vm = bind_vm(
            compile_with("use ngx;\nngx::sleep(60000);\n", sleep_catalog()),
            rt.handle().clone(),
            false,
        );
        let status = vm.run().expect("run");
        assert!(
            matches!(status, VmStatus::Waiting(_)),
            "must be waiting before reset, got {status:?}"
        );
        let noop = Waker::noop();
        let mut cx = Context::from_waker(noop);
        assert!(
            matches!(vm.poll_waiting_host_op(&mut cx), Poll::Pending),
            "future must still be waiting before reset"
        );
        vm.reset_for_reuse().expect("reset");
        let poll = vm.poll_waiting_host_op(&mut cx);
        assert!(
            !matches!(poll, Poll::Pending),
            "reset must drop the waiting future: {poll:?}"
        );
    }

    #[test]
    fn waker_is_send_sync_without_nginx_pointers() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<crate::notify::CompletionSender>();
        assert_send_sync::<RequestIdentity>();
        let name = std::any::type_name::<crate::notify::NgxWake>();
        assert!(
            !name.contains("ngx_connection")
                && !name.contains("ngx_http")
                && !name.contains("ngx_pool")
                && !name.contains("*mut"),
            "waker must not mention NGINX/VM data: {name}"
        );
    }

    #[test]
    fn wake_during_initial_poll_only_queues() {
        struct WakeThenPending;
        impl Future for WakeThenPending {
            type Output = Result<HostFutureOutput, VmError>;
            fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }

        let rt = runtime();
        let mut bridge = NgxAsyncBridge::new(rt.handle().clone());
        bridge
            .submit_op(1, Box::pin(WakeThenPending))
            .expect("submit");
        let notify = NotifyBridge::new().unwrap();
        let identity = RequestIdentity::new(9, 1);
        let waker = eventfd_waker(notify.sender(), identity);
        let mut cx = Context::from_waker(&waker);
        assert!(
            matches!(bridge.poll_submitted_op(1, &mut cx), Poll::Pending),
            "wake during poll must not complete the future on this stack"
        );
        assert!(
            bridge.contains(1),
            "wake during poll must not drop the future"
        );
        assert_eq!(
            notify.harvest_and_take_ready().unwrap(),
            vec![identity],
            "wake during poll must only queue for a later event turn"
        );
        waker.wake_by_ref();
        assert_eq!(
            notify.harvest_and_take_ready().unwrap(),
            vec![identity],
            "a cached waker must enqueue again after harvest"
        );
    }
}
