extern crate ini;
extern crate libc;

use self::ini::Ini;

#[macro_use]
mod ext;

use ffi::ext::*;

use self::libc::{c_char, c_void};

use std::ffi::CStr;

use crate::errors::*;

type Cell = isize;

static INVALID_CELL: Cell = 0;
use networking_queue::{Queue, RequestBuilder, RequestType, Response};
use std::prelude::v1::Vec;

use cell_map::CellMap;

struct ModuleStorage {
    pub global_queue: Queue,
    pub responses_handles: CellMap<Result<Response>>,
    pub bodies_handles: CellMap<Vec<u8>>,
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
    let ini = Ini::load_from_file(
        CStr::from_ptr(config_file_path as *const i8)
            .to_str()
            .unwrap(),
    )
    .map_err(|e| {
        println!(
            "Error: Can't parse/open grip config. Examine carefully ini parser log message\n{}",
            e
        );
        e
    })
    .unwrap();

    let dns_section = ini
        .section(Some("dns".to_owned()))
        .or_else(|| {
            println!("Missing [dns] section in the grip.ini config");
            None
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
        global_queue: Queue::new(
            dns_section
                .get("number-of-dns-threads")
                .or_else(|| {
                    println!(
                        "Error: Missing \"dns.number-of-dns-threads\" key in the grip.ini config"
                    );
                    None
                })
                .unwrap()
                .parse()
                .unwrap(),
        ),
        responses_handles: CellMap::new(),
        bodies_handles: CellMap::new(),
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

    return 1;
}

#[no_mangle]
pub unsafe extern "C" fn grip_body_from_string(amx: *const c_void, str: *const c_char) -> Cell {
    get_module_mut().bodies_handles.insert_with_unique_id(
        CStr::from_ptr(try_and_log_ffi!(
            amx,
            handle_null_ptr(str).chain_err(|| ffi_error("Invalid URI."))
        ))
        .to_bytes()
        .iter()
        .map(|i| *i)
        .collect(),
    )
}

#[no_mangle]
pub unsafe extern "C" fn grip_request(
    amx: *const c_void,
    forward_id: Cell,
    uri: *const c_char,
    request_type: Cell,
    body_handle: Cell,
    handler: Option<
        extern "C" fn(
            forward_handle: Cell,
            response_handle: Cell,
            user_data: *const Cell,
            user_data_size: Cell,
        ) -> c_void,
    >,
    user_data: *const Cell,
    user_data_size: Cell,
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
            handle_null_ptr(uri).chain_err(|| ffi_error("Invalid URI."))
        ))
        .to_str()
        .map_err(|_| ffi_error("URI is not UTF-8"))
    );

    let user_data: Vec<Cell> = std::slice::from_raw_parts(
        try_and_log_ffi!(
            amx,
            handle_null_ptr(user_data).chain_err(|| ffi_error("Invalid user data"))
        ),
        user_data_size as usize,
    )
    .to_vec();

    let body = try_and_log_ffi!(
        amx,
        get_module()
            .bodies_handles
            .get_with_id(body_handle)
            .or_else(|| if body_handle == -1 {
                lazy_static! {
                    static ref empty_vec: Vec<u8> = vec![];
                }
                Some(&empty_vec)
            } else {
                None
            })
            .chain_err(|| ffi_error(format!("Invalid body handle: {}", body_handle)))
    );

    // TODO: Handle cancellation in AMXX.
    // TODO: Get body in the AMXX.
    // TODO: Get error in AMXX
    get_module_mut().global_queue.send_request(
        RequestBuilder::default()
            .http_type(request_type)
            .body(body.clone())
            .uri(try_and_log_ffi!(
                amx,
                uri.parse()
                    .chain_err(|| ffi_error(format!("URI parsing error: {}", uri)))
            ))
            .build()
            .unwrap(),
        move |response| {
            let response_id = get_module_mut()
                .responses_handles
                .insert_with_unique_id(response);

            handler.unwrap()(
                forward_id,
                response_id,
                user_data.as_ptr(),
                user_data_size as isize,
            );

            get_module_mut()
                .responses_handles
                .remove_with_id(response_id);
        },
    );

    // TODO: Request handle
    1
}

#[no_mangle]
pub unsafe extern "C" fn grip_process_request() {
    get_module_mut().global_queue.execute_queue_with_limit(
        get_module().callbacks_per_frame,
        std::time::Duration::from_micros(get_module().microseconds_delay_between_attempts as u64),
    );
}