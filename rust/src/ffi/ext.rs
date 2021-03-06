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

use crate::errors::*;
use serde_json::Value;
use std::ffi::CStr;
use std::os::raw::c_char;

pub trait ResultFFIExt<T> {
    fn get_value(self) -> std::result::Result<T, String>;
}

impl<T> ResultFFIExt<T> for Result<T> {
    fn get_value(self) -> std::result::Result<T, String> {
        use error_chain::ChainedError;
        self.map_err(|e| format!("{}", e.display_chain()))
    }
}

impl<T> ResultFFIExt<T> for Option<T> {
    fn get_value(self) -> std::result::Result<T, String> {
        self.ok_or_else(|| "Got empty option".to_owned())
    }
}

macro_rules! try_and_log_ffi {
    ($amx:expr, $expr:expr, $error_logger:expr) => {
        match $expr.get_value() {
            std::result::Result::Ok(val) => val,
            std::result::Result::Err(err) => {
                ($error_logger)($amx, err);
                return 0;
            }
        }
    };

    ($amx:expr, $expr:expr) => {
        try_and_log_ffi!($amx, $expr, |amx, err| {
            (get_module().error_logger)(amx, format!("{}\0", err).as_ptr() as *const c_char);
        });
    };
}

pub fn ptr_to_option<T>(ptr: *const T) -> Option<*const T> {
    if ptr.is_null() {
        None
    } else {
        Some(ptr)
    }
}

pub unsafe fn str_from_ptr<'a>(value: *const c_char) -> Result<&'a str> {
    CStr::from_ptr(value)
        .to_str()
        .chain_err(|| "Can't create string from raw pointer.")
}

macro_rules! try_as_usize {
    ($amx:expr, $size:expr, $error_logger:expr) => {
        try_and_log_ffi!(
            $amx,
            if $size >= 0 {
                Ok($size as usize)
            } else {
                Err(ffi_error(format!(
                    "Index/Size {} should be greater or equal to zero.",
                    $size
                )))
            },
            $error_logger
        )
    };

    ($amx:expr, $size:expr) => {
        try_as_usize!($amx, $size, |amx, err| {
            (get_module().error_logger)(amx, format!("{}\0", err).as_ptr() as *const c_char);
        })
    };
}

macro_rules! copy_unsafe_string {
    ($amx:expr, $dest:expr, $source:expr, $size:expr, $error_logger:expr) => {{
        let source = format!("{}\0", $source);
        libc::strncpy(
            $dest,
            source.as_ptr() as *const c_char,
            try_as_usize!($amx, $size, $error_logger),
        );

        *$dest.offset($size) = '\0' as i8;

        std::cmp::min($size, source.len() as isize)
    }};

    ($amx:expr, $dest:expr, $source:expr, $size:expr) => {
        copy_unsafe_string!($amx, $dest, $source, $size, |amx, err| {
            (get_module().error_logger)(amx, format!("{}\0", err).as_ptr() as *const c_char);
        })
    };
}

macro_rules! unconditionally_log_error {
    ($amx:expr, $err:expr, $error_logger:expr) => {
        try_and_log_ffi!($amx, Err($err), $error_logger)
    };

    ($amx:expr, $err:expr) => {
        unconditionally_log_error!($amx, $err, |amx, err| {
            (get_module().error_logger)(amx, format!("{}\0", err).as_ptr() as *const c_char);
        })
    };
}

macro_rules! try_to_get_json_value {
    ($amx:expr, $value:expr) => {{
        let value: &Value = try_and_log_ffi!(
            $amx,
            get_module_mut()
                .json_handles
                .get_with_id($value)
                .chain_err(|| ffi_error(format!("Invalid JSON value handle {}", $value)))
        );

        value
    }};
}

macro_rules! try_to_get_json_value_mut {
    ($amx:expr, $value:expr) => {{
        let value: &mut Value = try_and_log_ffi!(
            $amx,
            get_module_mut()
                .json_handles
                .get_mut_with_id($value)
                .chain_err(|| ffi_error(format!("Invalid JSON value handle {}", $value)))
        );

        value
    }};
}

macro_rules! try_to_get_json_object_value {
    ($amx:expr, $object:expr, $name:expr, $dot_notation:expr) => {{
        try_and_log_ffi!(
            $amx,
            try_to_get_json_value!($amx, $object)
                .index_selective_safe(try_and_log_ffi!($amx, str_from_ptr($name)), $dot_notation)
        )
    }};
}

pub trait ValueExt<'a>: std::ops::Index<&'a str, Output = Value> {
    fn dot_index_safe(&self, name: &str) -> Result<&Value>;
    fn dot_index_safe_mut(&mut self, name: &str) -> Result<&mut Value>;

    fn index_selective_safe(&self, name: &'a str, dot_notation: bool) -> Result<&Value>;
    fn index_selective_safe_mut(&mut self, name: &'a str, dot_notation: bool)
        -> Result<&mut Value>;
}

impl<'a> ValueExt<'a> for Value {
    fn dot_index_safe(&self, name: &str) -> Result<&Value> {
        let mut it = self;
        for element in name.split('.') {
            if element.is_empty() {
                bail!("Double/Empty separator in `{}`", name);
            }

            // Same as bounds checked index.
            it = it.index_selective_safe(element, false)?;
        }

        Ok(it)
    }

    fn dot_index_safe_mut(&mut self, name: &str) -> Result<&mut Value> {
        let mut it = self;
        for element in name.split('.') {
            if element.is_empty() {
                bail!("Double/Empty separator in `{}`", name);
            }

            // Same as bounds checked index.
            it = it.index_selective_safe_mut(element, false)?;
        }

        Ok(it)
    }

    fn index_selective_safe(&self, name: &'a str, dot_notation: bool) -> Result<&Value> {
        if dot_notation {
            self.dot_index_safe(name)
        } else {
            match self {
                Value::Object(m) => {
                    if let Some(val) = m.get(name) {
                        Ok(val)
                    } else {
                        bail!(
                            "Can't index json using `{}`, because json doesn't contain it",
                            name
                        )
                    }
                }
                _ => bail!(
                    "Can't index json using `{}` json stops is not object.",
                    name
                ),
            }
        }
    }

    fn index_selective_safe_mut(
        &mut self,
        name: &'a str,
        dot_notation: bool,
    ) -> Result<&mut Value> {
        if dot_notation {
            self.dot_index_safe_mut(name)
        } else {
            match self {
                Value::Object(m) => {
                    if let Some(val) = m.get_mut(name) {
                        Ok(val)
                    } else {
                        bail!(
                            "Can't index json using `{}`, because json doesn't contain it",
                            name
                        )
                    }
                }
                _ => bail!(
                    "Can't index json using `{}` json stops is not object.",
                    name
                ),
            }
        }
    }
}

#[allow(unused_imports)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::Cell;
    use libc::c_char;
    use serde_json::json;

    unsafe fn copy_unsafe_string(size: isize) -> Cell {
        let mut s: [c_char; 2] = [0; 2];

        let status =
            copy_unsafe_string!(123 as *mut c_char, s.as_mut_ptr(), "1", size, |amx, _| {
                assert!(amx == 123 as *mut c_char);
            });

        assert_eq!(s, ['1' as c_char, '\0' as c_char]);

        status
    }

    #[test]
    fn copy_unsafe_string_test() {
        unsafe {
            assert_eq!(copy_unsafe_string(-1), 0);
            assert_eq!(copy_unsafe_string(2), 2);
        }
    }

    #[test]
    fn dot_index_safe() {
        let mut json = json!({
            "a": {
                "b": 123
            }
        });

        assert_eq!(json.dot_index_safe("a.b").unwrap().as_u64().unwrap(), 123);
        assert!(json.dot_index_safe("a.b.c").is_err());
        assert!(json.dot_index_safe("a..").is_err());
        assert!(json.dot_index_safe("a").unwrap().is_object());

        assert!(json.dot_index_safe_mut("a.b.c").is_err());
        assert_eq!(
            json.dot_index_safe_mut("a.b").unwrap().as_u64().unwrap(),
            123
        );
        assert!(json.dot_index_safe_mut("a..").is_err());
        assert!(json.dot_index_safe_mut("a").unwrap().is_object());

        assert_eq!(
            json.index_selective_safe("a.b", true)
                .unwrap()
                .as_u64()
                .unwrap(),
            123
        );
        assert!(json.index_selective_safe("a.b.c", true).is_err());
        assert!(json.index_selective_safe("a..", true).is_err());
        assert!(json.index_selective_safe("a", true).unwrap().is_object());

        assert_eq!(
            json.index_selective_safe_mut("a.b", true)
                .unwrap()
                .as_u64()
                .unwrap(),
            123
        );
        assert!(json.index_selective_safe_mut("a.b.c", true).is_err());
        assert!(json.index_selective_safe_mut("a..", true).is_err());
        assert!(json
            .index_selective_safe_mut("a", true)
            .unwrap()
            .is_object());

        assert!(json.index_selective_safe("a", false).unwrap().is_object());
        assert!(json.index_selective_safe("a.b.c", false).is_err());

        assert!(json
            .index_selective_safe_mut("a", false)
            .unwrap()
            .is_object());
        assert!(json.index_selective_safe_mut("a.b.c", false).is_err());
    }

}
