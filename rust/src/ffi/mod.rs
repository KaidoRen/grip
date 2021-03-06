/*
 * gRIP
 * Copyright (c) 2018 Alik Aslanyan <cplusplus256@gmail.com>
 *
 *
 *    This program is free software; you can redistribute it and/or modify it
 *    under the terms of the GNU General Public License as published by the
 *    Free Software Foundation; either version 3 of the License, or (at
 *    your option) any later version.
 *
 *    This program is distributed in the hope that it will be useful, but
 *    WITHOUT ANY WARRANTY; without even the implied warranty of
 *    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
 *    General Public License for more details.
 *
 *    You should have received a copy of the GNU General Public License
 *    along with this program; if not, write to the Free Software Foundation,
 *    Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 *
 *    In addition, as a special exception, the author gives permission to
 *    link the code of this program with the Half-Life Game Engine ("HL
 *    Engine") and Modified Game Libraries ("MODs") developed by Valve,
 *    L.L.C ("Valve").  You must obey the GNU General Public License in all
 *    respects for all of the code used other than the HL Engine and MODs
 *    from Valve.  If you modify this file, you may extend this exception
 *    to your version of the file, but you are not obligated to do so.  If
 *    you do not wish to do so, delete this exception statement from your
 *    version.
 *
 */

extern crate ini;
extern crate libc;

use self::ini::Ini;

#[macro_use]
mod ext;

use serde_json::json;

use crate::ffi::ext::*;

use self::libc::{c_char, c_void};

use std::ffi::CStr;
use std::fs::File;
use std::io::BufReader;

use crate::errors::*;
use lazy_static::*;

type Cell = isize;

use crate::networking_queue::{
    Queue, RequestBuilder, RequestCancellation, RequestOptions, RequestType, Response,
};
use std::prelude::v1::Vec;

use crate::cell_map::CellMap;
use serde_json::Value;

struct ModuleStorage {
    pub global_queue: Queue,
    pub current_response: Option<Result<Response>>,
    pub bodies_handles: CellMap<Vec<u8>>,
    pub cancellations_handles: CellMap<RequestCancellation>,
    pub json_handles: CellMap<Value>,
    pub options_handles: CellMap<RequestOptions>,
    pub error_logger: extern "C" fn(*const c_void, *const c_char),
    pub callbacks_per_frame: usize,
    pub microseconds_delay_between_attempts: usize,
}

static mut MODULE: Option<ModuleStorage> = None;

#[no_mangle]
pub unsafe extern "C" fn grip_init(
    error_logger: extern "C" fn(*const c_void, *const c_char),
    config_file_path: *const c_char,
) {
    if MODULE.is_some() {
        return;
    }

    let ini = Ini::load_from_file(str_from_ptr(config_file_path).unwrap())
        .map_err(|e| {
            println!(
                "Error: Can't parse/open grip config. Examine carefully ini parser log message\n{}",
                e
            );
            e
        })
        .unwrap();

    let queue_section = ini
        .section(Some("queue".to_owned()))
        .or_else(|| {
            println!("Error: Missing [queue] section in the grip.ini config");
            None
        })
        .unwrap();

    MODULE = Some(ModuleStorage {
        global_queue: Queue::new(),
        cancellations_handles: CellMap::new(),
        current_response: None,
        bodies_handles: CellMap::new(),
        json_handles: CellMap::new(),
        options_handles: CellMap::new(),
        error_logger,
        callbacks_per_frame: {
            queue_section
                .get("callbacks-per-frame")
                .or_else(|| {
                    println!(
                        "Error: Missing \"queue.callbacks-per-frame\" key in the grip.ini config"
                    );
                    None
                })
                .unwrap()
                .parse()
                .unwrap()
        },
        microseconds_delay_between_attempts: {
            queue_section
                .get("microseconds-delay-between-attempts")
                .or_else(|| {
                    println!("Error: Missing \"queue.microseconds-delay-between-attempts\" key in the grip.ini config");
                    None
                }).unwrap()
                .parse()
                .unwrap()
        },
    });
}

unsafe fn get_module() -> &'static ModuleStorage {
    MODULE.as_ref().unwrap()
}

unsafe fn get_module_mut() -> &'static mut ModuleStorage {
    MODULE.as_mut().unwrap()
}

#[no_mangle]
pub unsafe extern "C" fn grip_deinit() {
    if MODULE.is_some() {
        get_module_mut().cancellations_handles.clear(); // Cancel all operations, before queue stopped.
    }
    MODULE = None;
}

#[no_mangle]
pub unsafe extern "C" fn grip_destroy_body(amx: *const c_void, body: Cell) -> Cell {
    try_and_log_ffi!(
        amx,
        get_module_mut()
            .bodies_handles
            .remove_with_id(body)
            .chain_err(|| ffi_error(format!("Invalid body handle {}", body)))
    );

    1
}

#[no_mangle]
pub unsafe extern "C" fn grip_body_from_string(amx: *const c_void, str: *const c_char) -> Cell {
    get_module_mut().bodies_handles.insert_with_unique_id(
        CStr::from_ptr(try_and_log_ffi!(
            amx,
            ptr_to_option(str).chain_err(|| ffi_error("Invalid URI."))
        ))
        .to_bytes()
        .to_vec(),
    )
}

#[no_mangle]
pub unsafe extern "C" fn grip_request(
    amx: *const c_void,
    forward_id: Cell,
    uri: *const c_char,
    body_handle: Cell,
    request_type: Cell,
    handler: Option<extern "C" fn(forward_handle: Cell, user_data: Cell) -> c_void>,
    options_handle: Cell,
    user_data: Cell,
) -> Cell {
    let request_type = try_and_log_ffi!(
        amx,
        match request_type {
            0 => Ok(RequestType::Get),
            1 => Ok(RequestType::Post),
            2 => Ok(RequestType::Put),
            3 => Ok(RequestType::Delete),
            _ => Err(ErrorKind::FFIError(format!("Invalid request type {}", request_type)).into()),
        }
    );

    let uri = try_and_log_ffi!(
        amx,
        CStr::from_ptr(try_and_log_ffi!(
            amx,
            ptr_to_option(uri).chain_err(|| ffi_error("Invalid URI."))
        ))
        .to_str()
        .map_err(|_| ffi_error("URI is not UTF-8"))
    );

    let body = try_and_log_ffi!(
        amx,
        get_module()
            .bodies_handles
            .get_with_id(body_handle)
            .or_else(|| if body_handle == -1 {
                lazy_static! {
                    static ref EMPTY_VEC: Vec<u8> = vec![];
                }
                Some(&EMPTY_VEC)
            } else {
                None
            })
            .chain_err(|| ffi_error(format!("Invalid body handle: {}", body_handle)))
    );

    let options = try_and_log_ffi!(
        amx,
        get_module()
            .options_handles
            .get_with_id(options_handle)
            .or_else(|| if options_handle == -1 {
                lazy_static! {
                    static ref EMPTY_OPTIONS: RequestOptions = RequestOptions::default();
                }
                Some(&EMPTY_OPTIONS)
            } else {
                None
            })
            .chain_err(|| ffi_error(format!("Invalid options handle: {}", options_handle)))
    );

    // TODO: JSON

    let next_cancellation_id = get_module().cancellations_handles.peek_id();
    let cancellation = get_module_mut().global_queue.send_request(
        RequestBuilder::default()
            .http_type(request_type)
            .body(body.clone())
            .uri(try_and_log_ffi!(
                amx,
                uri.parse()
                    .chain_err(|| ffi_error(format!("URI parsing error: {}", uri)))
            ))
            .options(options.clone())
            .build()
            .unwrap(),
        move |response| {
            get_module_mut().current_response = Some(response);

            handler.unwrap()(forward_id, user_data);

            get_module_mut()
                .cancellations_handles
                .remove_with_id(next_cancellation_id);

            get_module_mut().current_response = None;
        },
    );

    get_module_mut()
        .cancellations_handles
        .insert_with_unique_id(cancellation)
}

//cell grip_cancel_request(const void* amx, cell cancellation);
#[no_mangle]
pub unsafe extern "C" fn grip_cancel_request(amx: *const c_void, cancellation: Cell) -> Cell {
    try_and_log_ffi!(
        amx,
        get_module_mut()
            .cancellations_handles
            .remove_with_id(cancellation)
            .chain_err(|| ffi_error(format!(
                "Cancellation with the id {} doesn't exist.",
                cancellation
            )))
    );

    1
}

#[no_mangle]
pub unsafe extern "C" fn grip_get_response_state(amx: *const c_void) -> Cell {
    match try_and_log_ffi!(
        amx,
        get_module()
            .current_response
            .as_ref()
            .chain_err(|| ffi_error("Response state can only be received in the request callback"))
    ) {
        Err(e) => match e.kind() {
            crate::errors::ErrorKind::RequestCancelled => 1,
            crate::errors::ErrorKind::RequestTimeout => 4,
            _ => 2,
        },
        Ok(_) => 3,
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_is_request_active(request_id: Cell) -> Cell {
    if get_module()
        .cancellations_handles
        .get_with_id(request_id)
        .is_some()
    {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_get_error_description(
    amx: *const c_void,
    buffer: *mut c_char,
    size: Cell,
) -> Cell {
    if let Err(e) = try_and_log_ffi!(
        amx,
        get_module()
            .current_response
            .as_ref()
            .chain_err(|| ffi_error("No active response at this time"))
    ) {
        try_and_log_ffi!(
            amx,
            match e.kind() {
                ErrorKind::RequestCancelled => Err(ErrorKind::RequestCancelled.into()),
                _ => Ok(()),
            }
        );

        use error_chain::ChainedError;
        copy_unsafe_string!(amx, buffer, e.display_chain(), size)
    } else {
        try_and_log_ffi!(amx, Err(ffi_error("No error for this response.")))
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_get_response_body_string(
    amx: *const c_void,
    buffer: *mut c_char,
    size: Cell,
) -> Cell {
    if let Ok(response) = try_and_log_ffi!(
        amx,
        get_module()
            .current_response
            .as_ref()
            .chain_err(|| ffi_error("No active response at this time"))
    ) {
        copy_unsafe_string!(
            amx,
            buffer,
            try_and_log_ffi!(
                amx,
                std::str::from_utf8(&response.body[..])
                    .chain_err(|| ffi_error("Unable to parse UTF-8"))
            ),
            size
        )
    } else {
        unconditionally_log_error!(
            amx,
            ffi_error("Error/Cancellation/Timeout occurred for this response.")
        )
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_get_response_status_code(amx: *const c_void) -> Cell {
    if let Ok(response) = try_and_log_ffi!(
        amx,
        get_module()
            .current_response
            .as_ref()
            .chain_err(|| ffi_error("No active response at this time"))
    ) {
        response.status_code.as_u16() as Cell
    } else {
        unconditionally_log_error!(
            amx,
            ffi_error("Error/Cancellation/Timeout occurred for this response.")
        );
        -1
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_destroy_json_value(amx: *const c_void, json_value: Cell) -> Cell {
    try_and_log_ffi!(
        amx,
        get_module_mut()
            .json_handles
            .remove_with_id(json_value)
            .chain_err(|| ffi_error(format!("Invalid json value handle {}", json_value)))
    );

    1
}

#[no_mangle]
pub unsafe extern "C" fn grip_create_default_options(amx: *const c_void, timeout: f64) -> Cell {
    use float_cmp::ApproxEq;

    get_module_mut()
        .options_handles
        .insert_with_unique_id(RequestOptions::new(
            reqwest::header::HeaderMap::default(),
            try_and_log_ffi!(
                amx,
                if timeout.approx_eq(&-1.0, std::f64::EPSILON, 2) {
                    Ok(None)
                } else if timeout >= 0.0 {
                    Ok(Some(std::time::Duration::from_millis(
                        (timeout * 1000.0) as u64,
                    )))
                } else {
                    Err(ffi_error(format!("Invalid timeout: {}", timeout)))
                }
            ),
        ))
}

#[no_mangle]
pub unsafe extern "C" fn grip_destroy_options(amx: *const c_void, options_handle: Cell) -> Cell {
    try_and_log_ffi!(
        amx,
        get_module_mut()
            .options_handles
            .remove_with_id(options_handle)
            .chain_err(|| ffi_error(format!("Invalid options handle {}", options_handle)))
    );

    1
}

#[no_mangle]
pub unsafe extern "C" fn grip_options_add_header(
    amx: *const c_void,
    options_handle: Cell,
    header_name: *const c_char,
    header_value: *const c_char,
) -> Cell {
    let option = try_and_log_ffi!(
        amx,
        get_module_mut()
            .options_handles
            .get_mut_with_id(options_handle)
            .chain_err(|| ffi_error(format!("Invalid options handle: {}", options_handle)))
    );

    let header_name = try_and_log_ffi!(
        amx,
        CStr::from_ptr(header_name)
            .to_str()
            .chain_err(|| ffi_error("Invalid header name. Can't create UTF-8 string"))
    );

    let header_value = try_and_log_ffi!(
        amx,
        str_from_ptr(header_value)
            .chain_err(|| ffi_error("Invalid header value. Can't create UTF-8 string"))
    );

    let header_value = try_and_log_ffi!(
        amx,
        reqwest::header::HeaderValue::from_str(header_value)
        .chain_err(|| ffi_error(format!("Header value contains invalid byte sequences or was rejected by Hyper HTTP implementation: {}", header_value)))
    );

    option.headers.insert(header_name, header_value);

    1
}

#[no_mangle]
pub unsafe extern "C" fn grip_process_request() {
    let multiplier = std::cmp::min(
        get_module().global_queue.number_of_pending_requests() / 500,
        1,
    );
    if multiplier > 1 {
        println!("[gRIP] Warning: More than 500 requests are pending.. Fastening execution {} times to compensate that", multiplier);
    }

    get_module_mut().global_queue.execute_queue_with_limit(
        get_module().callbacks_per_frame * multiplier,
        std::time::Duration::from_micros(get_module().microseconds_delay_between_attempts as u64),
    );
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_parse_response_body(
    amx: *const c_void,
    error_buffer: *mut c_char,
    error_buffer_size: Cell,
) -> Cell {
    if let Ok(response) = try_and_log_ffi!(
        amx,
        get_module()
            .current_response
            .as_ref()
            .chain_err(|| ffi_error("No active response at this time"))
    ) {
        let value: Result<serde_json::Value> =
            serde_json::from_slice(&response.body[..]).map_err(|e| ErrorKind::JSONError(e).into());

        match value {
            Ok(value) => get_module_mut().json_handles.insert_with_unique_id(value),
            Err(error) => {
                use error_chain::ChainedError;
                copy_unsafe_string!(amx, error_buffer, error.display_chain(), error_buffer_size);
                0
            }
        }
    } else {
        unconditionally_log_error!(
            amx,
            ffi_error("Error/Cancellation/Timeout occurred for this response.")
        )
    }
}

// TODO: Remove copy-paste
#[no_mangle]
pub unsafe extern "C" fn grip_json_parse_string(
    amx: *const c_void,
    string: *mut c_char,
    error_buffer: *mut c_char,
    error_buffer_size: Cell,
) -> Cell {
    let value: Result<serde_json::Value> = serde_json::from_str(try_and_log_ffi!(
        amx,
        CStr::from_ptr(string)
            .to_str()
            .chain_err(|| ffi_error("Invalid string. Can't create UTF-8 string"))
    ))
    .map_err(|e| ErrorKind::JSONError(e).into());

    match value {
        Ok(value) => get_module_mut().json_handles.insert_with_unique_id(value),
        Err(error) => {
            use error_chain::ChainedError;
            copy_unsafe_string!(amx, error_buffer, error.display_chain(), error_buffer_size);
            0
        }
    }
}

// TODO: Remove copy-paste
#[no_mangle]
pub unsafe extern "C" fn grip_json_parse_file(
    amx: *const c_void,
    file: *mut c_char,
    error_buffer: *mut c_char,
    error_buffer_size: Cell,
) -> Cell {
    let value: Result<serde_json::Value> =
        serde_json::from_reader(BufReader::new(try_and_log_ffi!(
            amx,
            File::open(try_and_log_ffi!(
                amx,
                CStr::from_ptr(file)
                    .to_str()
                    .chain_err(|| ffi_error("Invalid string. Can't create UTF-8 string"))
            ))
            .chain_err(|| ffi_error("Can't open file."))
        )))
        .map_err(|e| ErrorKind::JSONError(e).into());

    match value {
        Ok(value) => get_module_mut().json_handles.insert_with_unique_id(value),
        Err(error) => {
            use error_chain::ChainedError;
            copy_unsafe_string!(amx, error_buffer, error.display_chain(), error_buffer_size);
            0
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_equals(amx: *const c_void, value1: Cell, value2: Cell) -> Cell {
    let value1 = try_and_log_ffi!(
        amx,
        get_module()
            .json_handles
            .get_with_id(value1)
            .chain_err(|| ffi_error(format!("value1 handle {} is invalid", value1)))
    );

    let value2 = try_and_log_ffi!(
        amx,
        get_module()
            .json_handles
            .get_with_id(value2)
            .chain_err(|| ffi_error(format!("value2 {} handle is invalid", value2)))
    );

    if value2 == value1 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_get_type(amx: *const c_void, value: Cell) -> Cell {
    match try_and_log_ffi!(
        amx,
        get_module()
            .json_handles
            .get_with_id(value)
            .chain_err(|| ffi_error(format!("value {} handle is invalid", value)))
    ) {
        Value::Null => 1,
        Value::String(_) => 2,
        Value::Number(_) => 3,
        Value::Object(_) => 4,
        Value::Array(_) => 5,
        Value::Bool(_) => 6,
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_init_object() -> Cell {
    get_module_mut()
        .json_handles
        .insert_with_unique_id(json!({}))
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_init_array() -> Cell {
    get_module_mut()
        .json_handles
        .insert_with_unique_id(json!([]))
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_init_string(amx: *const c_void, string: *mut c_char) -> Cell {
    get_module_mut()
        .json_handles
        .insert_with_unique_id(json!(try_and_log_ffi!(
            amx,
            CStr::from_ptr(string)
                .to_str()
                .chain_err(|| ffi_error("Invalid string. Can't create UTF-8 string"))
        )
        .to_owned()))
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_init_number(value: Cell) -> Cell {
    get_module_mut()
        .json_handles
        .insert_with_unique_id(json!(value))
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_init_float(value: f64) -> Cell {
    get_module_mut()
        .json_handles
        .insert_with_unique_id(json!(value))
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_init_bool(value: bool) -> Cell {
    get_module_mut()
        .json_handles
        .insert_with_unique_id(json!(value))
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_init_null() -> Cell {
    get_module_mut()
        .json_handles
        .insert_with_unique_id(json!(null))
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_get_string(
    amx: *const c_void,
    value: Cell,
    buffer: *mut c_char,
    buffer_size: Cell,
) -> Cell {
    match try_to_get_json_value!(amx, value) {
        Value::String(s) => copy_unsafe_string!(amx, buffer, s, buffer_size),
        v => unconditionally_log_error!(
            amx,
            ffi_error(format!("JSON Handle is not string. {:?}", v))
        ),
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_get_number(amx: *const c_void, value: Cell) -> Cell {
    match try_to_get_json_value!(amx, value) {
        Value::Number(n) => try_and_log_ffi!(
            amx,
            n.as_i64().chain_err(|| ffi_error("Number is not integer"))
        ) as Cell,
        v => unconditionally_log_error!(
            amx,
            ffi_error(format!("JSON Handle is not number. {:?}", v))
        ),
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_get_float(
    amx: *const c_void,
    value: Cell,
    ret: *mut f32,
) -> Cell {
    *ret = 0.0;

    match try_to_get_json_value!(amx, value) {
        Value::Number(n) => {
            *ret = try_and_log_ffi!(
                amx,
                n.as_f64()
                    .chain_err(|| ffi_error("Number is not 32 bit float"))
            ) as f32;
            1
        }
        v => unconditionally_log_error!(
            amx,
            ffi_error(format!("JSON Handle is not number. {:?}", v))
        ),
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_get_bool(amx: *const c_void, value: Cell) -> Cell {
    match try_to_get_json_value!(amx, value) {
        Value::Bool(b) => *b as Cell,
        v => unconditionally_log_error!(
            amx,
            ffi_error(format!("JSON Handle is not bool. {:?}", v))
        ),
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_get_value(
    amx: *const c_void,
    array: Cell,
    index: Cell,
) -> Cell {
    match try_to_get_json_value!(amx, array) {
        Value::Array(vec) => get_module_mut()
            .json_handles
            .insert_with_unique_id(vec[try_as_usize!(amx, index)].clone()),
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_get_string(
    amx: *const c_void,
    array: Cell,
    index: Cell,
    buffer: *mut c_char,
    buffer_size: Cell,
) -> Cell {
    match try_to_get_json_value!(amx, array) {
        Value::Array(vec) => match &vec[try_as_usize!(amx, index)] {
            Value::String(s) => copy_unsafe_string!(amx, buffer, s, buffer_size),
            v => unconditionally_log_error!(
                amx,
                ffi_error(format!("JSON Handle is not string. {:?}", v))
            ),
        },
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_get_number(
    amx: *const c_void,
    array: Cell,
    index: Cell,
) -> Cell {
    match try_to_get_json_value!(amx, array) {
        Value::Array(vec) => match &vec[try_as_usize!(amx, index)] {
            Value::Number(n) => try_and_log_ffi!(
                amx,
                n.as_i64().chain_err(|| ffi_error("Number is not integer"))
            ) as Cell,
            v => unconditionally_log_error!(
                amx,
                ffi_error(format!("JSON Handle is not number. {:?}", v))
            ),
        },
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_get_float(
    amx: *const c_void,
    array: Cell,
    index: Cell,
    ret: *mut f32,
) -> Cell {
    match try_to_get_json_value!(amx, array) {
        Value::Array(vec) => match &vec[try_as_usize!(amx, index)] {
            Value::Number(n) => {
                *ret = try_and_log_ffi!(
                    amx,
                    n.as_f64()
                        .chain_err(|| ffi_error("Number is not 32 bit float"))
                ) as f32;

                1
            }
            v => unconditionally_log_error!(
                amx,
                ffi_error(format!("JSON Handle is not number. {:?}", v))
            ),
        },
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_get_bool(
    amx: *const c_void,
    array: Cell,
    index: Cell,
) -> Cell {
    match try_to_get_json_value!(amx, array) {
        Value::Array(vec) => match &vec[try_as_usize!(amx, index)] {
            Value::Bool(b) => *b as Cell,
            v => unconditionally_log_error!(
                amx,
                ffi_error(format!("JSON Handle is not number. {:?}", v))
            ),
        },
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_get_count(amx: *const c_void, array: Cell) -> Cell {
    match try_to_get_json_value!(amx, array) {
        Value::Array(vec) => vec.len() as Cell,
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_replace_value(
    amx: *const c_void,
    array: Cell,
    index: Cell,
    value: Cell,
) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec[try_as_usize!(amx, index)] = try_to_get_json_value!(amx, value).clone();
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_replace_string(
    amx: *const c_void,
    array: Cell,
    index: Cell,
    string: *const c_char,
) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec[try_as_usize!(amx, index)] = json!(try_and_log_ffi!(
                amx,
                CStr::from_ptr(string)
                    .to_str()
                    .chain_err(|| ffi_error("Invalid string. Can't create UTF-8 string"))
            )
            .to_owned());
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_replace_number(
    amx: *const c_void,
    array: Cell,
    index: Cell,
    value: Cell,
) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec[try_as_usize!(amx, index)] = json!(value);
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_replace_float(
    amx: *const c_void,
    array: Cell,
    index: Cell,
    value: f32,
) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec[try_as_usize!(amx, index)] = json!(value);
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_replace_bool(
    amx: *const c_void,
    array: Cell,
    index: Cell,
    value: bool,
) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec[try_as_usize!(amx, index)] = json!(value);
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_replace_null(
    amx: *const c_void,
    array: Cell,
    index: Cell,
) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec[try_as_usize!(amx, index)] = json!(null);
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_append_value(
    amx: *const c_void,
    array: Cell,
    value: Cell,
) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec.push(try_to_get_json_value!(amx, value).clone());
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_append_string(
    amx: *const c_void,
    array: Cell,
    string: *const c_char,
) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec.push(json!(try_and_log_ffi!(
                amx,
                CStr::from_ptr(string)
                    .to_str()
                    .chain_err(|| ffi_error("Invalid string. Can't create UTF-8 string"))
            )
            .to_owned()));
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_append_number(
    amx: *const c_void,
    array: Cell,
    value: Cell,
) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec.push(json!(value));
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_append_float(
    amx: *const c_void,
    array: Cell,
    value: f32,
) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec.push(json!(value));
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_append_bool(
    amx: *const c_void,
    array: Cell,
    value: bool,
) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec.push(json!(value));
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_append_null(amx: *const c_void, array: Cell) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec.push(json!(null));
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_remove(
    amx: *const c_void,
    array: Cell,
    index: Cell,
) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec.remove(try_and_log_ffi!(
                amx,
                if index >= 0 {
                    Ok(index as usize)
                } else {
                    Err(ffi_error(format!(
                        "Index/Size {} should be greater or equal to zero.",
                        index
                    )))
                }
            ));
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_array_clear(amx: *const c_void, array: Cell) -> Cell {
    match try_to_get_json_value_mut!(amx, array) {
        Value::Array(vec) => {
            vec.clear();
            1
        }
        v => {
            unconditionally_log_error!(amx, ffi_error(format!("JSON Handle is not array. {:?}", v)))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_object_get_value(
    amx: *const c_void,
    object: Cell,
    name: *const c_char,
    dot_notation: bool,
) -> Cell {
    get_module_mut().json_handles.insert_with_unique_id(
        try_to_get_json_object_value!(amx, object, name, dot_notation).clone(),
    )
}
#[no_mangle]
pub unsafe extern "C" fn grip_json_object_get_string(
    amx: *const c_void,
    object: Cell,
    name: *const c_char,
    buffer: *mut c_char,
    maxlen: Cell,
    dot_notation: bool,
) -> Cell {
    match try_to_get_json_object_value!(amx, object, name, dot_notation) {
        Value::String(s) => copy_unsafe_string!(amx, buffer, s, maxlen),
        v => unconditionally_log_error!(
            amx,
            ffi_error(format!("JSON Handle is not string. {:?}", v))
        ),
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_object_get_number(
    amx: *const c_void,
    object: Cell,
    name: *const c_char,
    dot_notation: bool,
) -> Cell {
    match try_to_get_json_object_value!(amx, object, name, dot_notation) {
        Value::Number(n) => try_and_log_ffi!(
            amx,
            n.as_i64().chain_err(|| ffi_error("Number is not integer"))
        ) as Cell,
        v => unconditionally_log_error!(
            amx,
            ffi_error(format!("JSON Handle is not number. {:?}", v))
        ),
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_object_get_float(
    amx: *const c_void,
    object: Cell,
    name: *const c_char,
    dot_notation: bool,
    ret: *mut f32,
) -> Cell {
    match try_to_get_json_object_value!(amx, object, name, dot_notation) {
        Value::Number(n) => {
            *ret = try_and_log_ffi!(
                amx,
                n.as_f64().chain_err(|| ffi_error("Number is not float"))
            ) as f32;

            1
        }
        v => unconditionally_log_error!(
            amx,
            ffi_error(format!("JSON Handle is not number. {:?}", v))
        ),
    }
}

#[no_mangle]
pub unsafe extern "C" fn grip_json_object_get_bool(
    amx: *const c_void,
    object: Cell,
    name: *const c_char,
    dot_notation: bool,
) -> Cell {
    match try_to_get_json_object_value!(amx, object, name, dot_notation) {
        Value::Bool(b) => *b as Cell,
        v => unconditionally_log_error!(
            amx,
            ffi_error(format!("JSON Handle is not bool. {:?}", v))
        ),
    }
}
