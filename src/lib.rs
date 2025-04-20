pub mod local_db_model;
pub mod local_db_state;
mod test;

use crate::local_db_model::LocalDbModel;
use crate::local_db_state::AppDbState;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use log::{info, warn};

#[no_mangle]
pub extern "C" fn create_db(name: *const c_char) -> *mut AppDbState {
    // Proteger contra punteros nulos
    if name.is_null() {
        eprintln!("Rust: NULL pointer passed to create_db");
        return std::ptr::null_mut();
    }

    // Convertir C string a Rust string de manera segura
    let name_str = match unsafe { CStr::from_ptr(name).to_str() } {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Rust: Invalid UTF-8 in database name: {}", e);
            return std::ptr::null_mut();
        }
    };

    // Usar una ruta absoluta o relativa consistente
    let db_path = format!("./{}", name_str);

    // Inicializar la base de datos y manejar el resultado
    match AppDbState::init(db_path) {
        Ok(state) => {
            println!("Rust: Database initialized successfully");
            Box::into_raw(Box::new(state))
        }
        Err(err) => {
            eprintln!("Rust: Failed to initialize database: {}", err);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn push_data(state: *mut AppDbState, json_ptr: *const c_char) -> *const c_char {
    let state = match unsafe { state.as_ref() } {
        Some(s) => s,
        None => {
            eprintln!("Error: null state pointer");
            return std::ptr::null();
        }
    };

    let json_str = match unsafe { CStr::from_ptr(json_ptr).to_str() } {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error converting C string: {}", e);
            return std::ptr::null();
        }
    };

    let model: LocalDbModel = match serde_json::from_str(json_str) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Error deserializing JSON: {}", e);
            eprintln!("Received JSON: {}", json_str);
            return std::ptr::null();
        }
    };

    match state.push(model) {
        Ok(result_model) => {
            match serde_json::to_string(&result_model) {
                Ok(json) => {
                    match CString::new(json) {
                        Ok(c_str) => c_str.into_raw(),
                        Err(e) => {
                            eprintln!("Error creating CString: {}", e);
                            std::ptr::null()
                        }
                    }
                },
                Err(e) => {
                    eprintln!("Error serializing result: {}", e);
                    std::ptr::null()
                }
            }
        },
        Err(e) => {
            eprintln!("Error pushing data: {:?}", e);
            std::ptr::null()
        }
    }
}


#[no_mangle]
pub extern "C" fn get_by_id(state: *mut AppDbState, id: *const c_char) -> *const c_char {
    // Verificar si state o id son nulos
    if state.is_null() {
        warn!("Rust: Null state pointer passed to get_by_id");
        return std::ptr::null();
    }

    if id.is_null() {
        warn!("Rust: Null id pointer passed to get_by_id");
        return std::ptr::null();
    }

    // Ahora es seguro desreferenciar
    let state = unsafe { &*state };

    // Convertir id a String con manejo de errores
    let id_str = match unsafe { CStr::from_ptr(id).to_str() } {
        Ok(s) => s,
        Err(e) => {
            warn!("Rust: Invalid UTF-8 in id: {:?}", e);
            return std::ptr::null();
        }
    };

    match state.get_by_id(id_str) {
        Ok(Some(model)) => {
            match serde_json::to_string(&model) {
                Ok(json) => {
                    match CString::new(json) {
                        Ok(c_string) => c_string.into_raw(),
                        Err(e) => {
                            warn!("Rust: Error creating CString: {:?}", e);
                            std::ptr::null()
                        }
                    }
                },
                Err(e) => {
                    println!("Rust: Error serializing to JSON: {:?}", e);
                    std::ptr::null()
                }
            }
        },
        Ok(None) => {
            println!("Rust: No model found with id: {}", id_str);
            std::ptr::null()
        },
        Err(e) => {
            println!("Rust: Error in get_by_id: {:?}", e);
            std::ptr::null()
        }
    }
}

#[no_mangle]
pub extern "C" fn get_all(state: *mut AppDbState) -> *const c_char {
    // Verificar si el puntero es nulo
    if state.is_null() {
        println!("Rust: Null state pointer passed to get_all");
        return std::ptr::null();
    }

    // Ahora es seguro desreferenciar
    let state = unsafe { &*state };
    match state.get() {
        Ok(models) => {
            let json = serde_json::to_string(&models).unwrap();
            CString::new(json).unwrap().into_raw()
        },
        Err(e) => {
            println!("Rust: Error in get_all: {:?}", e);
            std::ptr::null()
        }
    }
}


#[no_mangle]
pub extern "C" fn update_data(state: *mut AppDbState, json_ptr: *const c_char) -> *const c_char {
    // Verificar si los punteros son nulos
    if state.is_null() || json_ptr.is_null() {
        return std::ptr::null();
    }

    // Mantener el resto exactamente igual
    let state = unsafe { &*state };
    let json_str = unsafe { CStr::from_ptr(json_ptr).to_str().unwrap() };
    let model: LocalDbModel = serde_json::from_str(json_str).unwrap();

    match state.update(model) {
        Ok(Some(updated_model)) => {
            CString::new(serde_json::to_string(&updated_model).unwrap()).unwrap().into_raw()
        },
        _ => std::ptr::null()
    }
}

#[no_mangle]
pub extern "C" fn delete_by_id(db_state: *mut AppDbState, id: *const c_char) -> bool {
    // Verificar que los punteros no sean nulos
    if db_state.is_null() || id.is_null() {
        return false;
    }

    // Convertir el puntero de ID a un string de Rust
    let id_str = unsafe {
        CStr::from_ptr(id)
            .to_str()
            .unwrap_or("No se pudo pasar a String")
    };

    // Acceder al estado de la base de datos
    let db_state = unsafe { &mut *db_state };

    // Usar tu implementación existente de delete
    db_state.delete_by_id(id_str).unwrap_or_else(|_| false)
}

#[no_mangle]
pub extern "C" fn clear_all_records(db_state: &AppDbState) -> *const c_char {
    match db_state.clear_all_records() {
        Ok(response) => {
            let response_str = response.to_string();
            CString::new(response_str).unwrap().into_raw()
        }
        Err(e) => {
            println!("Rust: Error in clear all records: {:?}", e);
            CString::new("Error clearing data").unwrap().into_raw()
        }
    }
}



#[no_mangle]
pub extern "C" fn reset_database(db_state: &mut AppDbState, name: &String) -> *const c_char{
    match db_state.reset_database(name) {
        Ok(_) => {
            CString::new("Database was removed").unwrap().into_raw()
        },
        Err(e) => {
            println!("Rust: Error in reset database: {:?}", e); // Debug
            CString::new("Error clearing data").unwrap().into_raw()
        }
    }
}

#[no_mangle]
pub extern "C" fn close_database(db_ptr: *mut AppDbState) -> *mut bool {
    if !db_ptr.is_null() {
        unsafe {
            let _ = Box::from_raw(db_ptr);
            // Crear un nuevo booleano en el heap para devolver
            let success = Box::new(true);
            Box::into_raw(success)
        }
    } else {
        let failure = Box::new(false);
        Box::into_raw(failure)
    }
}

#[no_mangle]
pub extern "C" fn is_database_open(db_ptr: *const AppDbState) -> *mut bool {
    if !db_ptr.is_null() {
        unsafe {
            let db_state = &*db_ptr;
            
            // Devolver el resultado como un puntero
            let result = Box::new(db_state.is_open());
            Box::into_raw(result)
        }
    } else {
        let result = Box::new(false);
        Box::into_raw(result)
    }
}