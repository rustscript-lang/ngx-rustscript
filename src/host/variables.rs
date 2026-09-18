// `ngx.var` read adaptation. Lookup uses the public NGINX variable API.

use nginx_sys::{
    ngx_http_get_variable, ngx_http_request_t, ngx_http_variable_value_t, ngx_str_t, ngx_uint_t,
};
use ngx::http::Request;
use pd_host_function::pd_host_function;
use vm::HostModuleDescriptor;
use vm::{Value, VmError};

use super::super::VmResult;
use super::{CallOutcome, IntoHostCallOutcome};
use crate::host::{current_identity, with_current_request, with_current_request_mut, ScopeError};

const MAX_VAR_NAME: usize = 1_024;
const MAX_VAR_VALUE: usize = 65_536;

fn ret<T: IntoHostCallOutcome>(value: T) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn scope_err(err: ScopeError) -> VmError {
    VmError::HostError(err.to_string())
}

fn with_request<T>(f: impl FnOnce(&Request) -> VmResult<T>) -> VmResult<T> {
    let identity = current_identity().map_err(scope_err)?;
    with_current_request(identity, f).map_err(scope_err)?
}

fn with_request_write<T>(f: impl FnOnce(&mut Request) -> VmResult<T>) -> VmResult<T> {
    let identity = current_identity().map_err(scope_err)?;
    with_current_request_mut(identity, f).map_err(scope_err)?
}

unsafe extern "C" {
    fn ngx_rustscript_c_variable_set(
        r: *mut ngx_http_request_t,
        name: *mut u8,
        name_len: usize,
        value: *mut u8,
        value_len: usize,
        is_null: i32,
    ) -> i32;
    fn ngx_rustscript_c_variable_not_found(vv: *const ngx_http_variable_value_t) -> u32;
}

fn bytes_as_string(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => err.into_bytes().into_iter().map(char::from).collect(),
    }
}

fn hash_strlow(dst: &mut [u8], src: &[u8]) -> ngx_uint_t {
    let mut key: ngx_uint_t = 0;
    for (index, &byte) in src.iter().enumerate() {
        let low = byte.to_ascii_lowercase();
        dst[index] = low;
        key = key.wrapping_mul(31).wrapping_add(low as ngx_uint_t);
    }
    key
}

/// Read an NGINX variable. Missing/`not_found` is `null`; empty is `""`.
#[pd_host_function(name = "ngx::var::get", contract = crate::host::catalog::ngx_var_get_contract)]
pub fn get(name: &str) -> VmResult<CallOutcome> {
    if name.is_empty() {
        return Err(VmError::HostError(
            "ngx::var::get variable name is empty".to_string(),
        ));
    }
    if name.len() > MAX_VAR_NAME {
        return Err(VmError::HostError(format!(
            "ngx::var::get variable name exceeds {MAX_VAR_NAME} bytes"
        )));
    }
    if name.as_bytes().contains(&0) {
        return Err(VmError::HostError(
            "ngx::var::get variable name contains NUL".to_string(),
        ));
    }

    with_request(|request| {
        let mut low = vec![0u8; name.len()];
        let hash = hash_strlow(&mut low, name.as_bytes());
        let mut ns = ngx_str_t {
            len: low.len(),
            data: low.as_mut_ptr(),
        };
        let r = request.as_ref() as *const ngx_http_request_t as *mut ngx_http_request_t;
        // SAFETY: `r` is the live scoped request; `ns` is a stack ngx_str of the
        // lowercased name. nginx-sys documents ngx_http_get_variable as the
        // public lookup API.
        let vv = unsafe { ngx_http_get_variable(r, &mut ns, hash) };
        if vv.is_null() {
            return ret(Value::Null);
        }
        if unsafe { ngx_rustscript_c_variable_not_found(vv) } != 0 {
            return ret(Value::Null);
        }
        let vv = unsafe { &*vv };
        let len = vv.len() as usize;
        if vv.data.is_null() || len == 0 {
            return ret(String::new());
        }
        if len > MAX_VAR_VALUE {
            return Err(VmError::HostError(format!(
                "ngx::var::get value exceeds {MAX_VAR_VALUE} bytes"
            )));
        }
        let bytes = unsafe { std::slice::from_raw_parts(vv.data, len) }.to_vec();
        ret(bytes_as_string(bytes))
    })
}

/// Set an NGINX variable. `null` clears (`not_found`); empty is a present empty string.
#[pd_host_function(name = "ngx::var::set", contract = crate::host::catalog::ngx_var_set_contract)]
pub fn set(name: String, value: &Value) -> VmResult<CallOutcome> {
    if name.is_empty() {
        return Err(VmError::HostError(
            "ngx::var::set variable name is empty".to_string(),
        ));
    }
    if name.len() > MAX_VAR_NAME {
        return Err(VmError::HostError(format!(
            "ngx::var::set variable name exceeds {MAX_VAR_NAME} bytes"
        )));
    }
    if name.as_bytes().contains(&0) {
        return Err(VmError::HostError(
            "ngx::var::set variable name contains NUL".to_string(),
        ));
    }
    let (is_null, bytes) = match value {
        Value::Null => (1, Vec::new()),
        Value::String(text) => {
            if text.len() > MAX_VAR_VALUE {
                return Err(VmError::HostError(format!(
                    "ngx::var::set value exceeds {MAX_VAR_VALUE} bytes"
                )));
            }
            (0, text.as_bytes().to_vec())
        }
        _ => {
            return Err(VmError::HostError(
                "ngx::var::set value expects string or null".to_string(),
            ));
        }
    };
    let mut name_bytes = name.into_bytes();
    with_request_write(|request| {
        let r = request.as_mut() as *mut ngx_http_request_t;
        let mut value_bytes = bytes;
        let rc = unsafe {
            ngx_rustscript_c_variable_set(
                r,
                name_bytes.as_mut_ptr(),
                name_bytes.len(),
                if is_null == 1 || value_bytes.is_empty() {
                    std::ptr::null_mut()
                } else {
                    value_bytes.as_mut_ptr()
                },
                value_bytes.len(),
                is_null,
            )
        };
        match rc {
            0 => ret(()),
            1 => Err(VmError::HostError(
                "variable not found for writing".into(),
            )),
            2 => Err(VmError::HostError("variable not changeable".into())),
            3 => Err(VmError::HostError(
                "variable cannot be assigned".into(),
            )),
            4 => Err(VmError::HostError("out of memory".into())),
            5 => Err(VmError::HostError("request pool is null".into())),
            _ => Err(VmError::HostError("variable set failed".into())),
        }
    })
}


const VARIABLES_HOST_MODULE_FUNCTIONS: &[fn() -> vm::HostFunctionDescriptor] = &[
        get_descriptor,
        set_descriptor,
];

pub fn variables_host_module() -> HostModuleDescriptor {
    HostModuleDescriptor {
        name: "ngx.var",
        functions: VARIABLES_HOST_MODULE_FUNCTIONS,
        resources: &[],
    }
}
