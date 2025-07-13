pub mod local_db_model;
pub mod local_db_state;
mod test;
mod app_response;

use crate::local_db_model::LocalDbModel;
use crate::local_db_state::AppDbState;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use log::{info, warn};

use crate::app_response::AppResponse;

#[no_mangle]
pub extern "C" fn create_db(name: *const c_char) -> *mut AppDbState {
    // Verificar si el puntero es nulo
    if name.is_null() {
        warn!("Null name pointer passed to create_db");
        return std::ptr::null_mut();
    }

    // Manejo seguro de conversión UTF-8
    let name_str = match unsafe { CStr::from_ptr(name).to_str() } {
        Ok(s) => s,
        Err(e) => {
            warn!("Invalid UTF-8 in name parameter: {}", e);
            return std::ptr::null_mut();
        }
    };

    // Usar una ruta absoluta o relativa consistente
    let db_path = format!("./{}", name_str);

    // Verificar si la base de datos ya existe
    if std::path::Path::new(&db_path).exists() {
        info!("Rust: Database already exists, opening existing database");
    } else {
        info!("Rust: Creating new database");
    }

    let state = AppDbState::init(db_path);
    info!("Rust: Database initialized");
    
    match state {
        Ok(response) => {
            Box::into_raw(Box::new(response))
        }
        Err(_) => {
            std::ptr::null_mut()
        }
    }
}


#[no_mangle]
pub extern "C" fn push_data(state: *mut AppDbState, json_ptr: *const c_char) -> *const c_char {
    let state = match unsafe { state.as_ref() } {
        Some(s) => s,
        None => {
            let error = AppResponse::BadRequest("Null state pointer".to_string());
            return response_to_c_string(&error);
        }
    };

  
    let json_str = match c_ptr_to_string(json_ptr, "JSON") {
        Ok(response) => response,
        Err(err) => return err
    };

    let model: LocalDbModel = match serde_json::from_str(&*json_str) {
        Ok(m) => m,
        Err(e) => {
            let error = AppResponse::SerializationError(format!("Invalid JSON: {}", e));
            return response_to_c_string(&error);
        }
    };
    

    match state.push(model) {
        Ok(result_model) => {
            match serde_json::to_string(&result_model) {
                Ok(json) => {
                    let success = AppResponse::Ok(json);
                    response_to_c_string(&success)
                },
                Err(e) => {
                    let error = AppResponse::SerializationError(format!("Failed to serialize result: {}", e));
                    response_to_c_string(&error)
                }
            }
        },
        Err(e) => {
            // Aquí 'e' ya es un AppResponse
            response_to_c_string(&e)
        }
    }
}


#[no_mangle]
pub extern "C" fn get_by_id(state: *mut AppDbState, id: *const c_char) -> *const c_char {
    // Verificar si state o id son nulos
    if state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to get_by_id".to_string());
        return response_to_c_string(&error);
    }

    if id.is_null() {
        let error = AppResponse::BadRequest("Null id pointer passed to get_by_id".to_string());
        return response_to_c_string(&error);
    }

    // Ahora es seguro desreferenciar
    let state = unsafe { &*state };

    // Convertir id a String con manejo de errores

    let id_str = match c_ptr_to_string(id, "id") {
        Ok(json) => json,
        Err(error_ptr) => return error_ptr,
    };

    match state.get_by_id(&*id_str) {
        Ok(Some(model)) => {
            match serde_json::to_string(&model) {
                Ok(json) => {
                    let success = AppResponse::Ok(json);
                    response_to_c_string(&success)
                },
                Err(e) => {
                    let error = AppResponse::SerializationError(format!("Error serializing to JSON: {:?}", e));
                    response_to_c_string(&error)
                }
            }
        },
        Ok(None) => {
            let error = AppResponse::NotFound(format!("No model found with id: {}", id_str));
            response_to_c_string(&error)
        },
        Err(e) => {
            // Asumiendo que e ya es o puede convertirse a AppResponse
            let error = AppResponse::DatabaseError(format!("Error in get_by_id: {:?}", e));
            response_to_c_string(&error)
        }
    }
}

#[no_mangle]
pub extern "C" fn get_all(state: *mut AppDbState) -> *const c_char {
    // Verificar si el puntero es nulo
    if state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to get_all".to_string());
        return response_to_c_string(&error);
    }

    // Ahora es seguro desreferenciar
    let state = unsafe { &*state };

    match state.get() {
        Ok(models) => {
            match serde_json::to_string(&models) {
                Ok(json) => {
                    let success = AppResponse::Ok(json);
                    response_to_c_string(&success)
                },
                Err(e) => {
                    let error = AppResponse::SerializationError(format!("Error serializing models: {:?}", e));
                    response_to_c_string(&error)
                }
            }
        },
        Err(e) => {
            let error = AppResponse::DatabaseError(format!("Error in get_all: {:?}", e));
            response_to_c_string(&error)
        }
    }
}

#[no_mangle]
pub extern "C" fn update_data(state: *mut AppDbState, json_ptr: *const c_char) -> *const c_char {
    // Verificar si los punteros son nulos
    if state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to update_data".to_string());
        return response_to_c_string(&error);
    }

    if json_ptr.is_null() {
        let error = AppResponse::BadRequest("Null JSON pointer passed to update_data".to_string());
        return response_to_c_string(&error);
    }

    // Convertir el puntero C a string de Rust con manejo de errores
    let json_str = match c_ptr_to_string(json_ptr, "JSON") {
        Ok(json) => json,
        Err(error_ptr) => return error_ptr,
    };

    // Deserializar el JSON a modelo con manejo de errores
    let model: LocalDbModel = match serde_json::from_str(&*json_str) {
        Ok(m) => m,
        Err(e) => {
            let error = AppResponse::SerializationError(format!("Error deserializing JSON: {:?}", e));
            return response_to_c_string(&error);
        }
    };


    // Mantener el resto exactamente igual
    let state = unsafe { &*state };

    match state.update(model) {
        Ok(Some(updated_model)) => {
            // Serializar el modelo actualizado
            match serde_json::to_string(&updated_model) {
                Ok(json) => {
                    let success = AppResponse::Ok(json);
                    response_to_c_string(&success)
                },
                Err(e) => {
                    let error = AppResponse::SerializationError(format!("Error serializing updated model: {:?}", e));
                    response_to_c_string(&error)
                }
            }
        },
        Ok(None) => {
            let error = AppResponse::NotFound("Model not found for update".to_string());
            response_to_c_string(&error)
        },
        Err(e) => {
            let error = AppResponse::DatabaseError(format!("Error updating model: {:?}", e));
            response_to_c_string(&error)
        }
    }
}

#[no_mangle]
pub extern "C" fn delete_by_id(db_state: *mut AppDbState, id: *const c_char) -> *const c_char {
    // Verificar que los punteros no sean nulos
    if db_state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to delete_by_id".to_string());
        return response_to_c_string(&error);
    }

    if id.is_null() {
        let error = AppResponse::BadRequest("Null id pointer passed to delete_by_id".to_string());
        return response_to_c_string(&error);
    }

    // Convertir el puntero de ID a un string de Rust
    let id_str = match c_ptr_to_string(id, "id") {
        Ok(id) => id,
        Err(error_ptr) => return error_ptr,
    };

    // Acceder al estado de la base de datos
    let db_state = unsafe { &mut *db_state };

    // Usar tu implementación existente de delete con manejo adecuado de errores
    match db_state.delete_by_id(&*id_str) {
        Ok(true) => {
            let success = AppResponse::Ok("Record deleted successfully".to_string());
            response_to_c_string(&success)
        },
        Ok(false) => {
            let not_found = AppResponse::NotFound(format!("No record found with id: {}", id_str));
            response_to_c_string(&not_found)
        },
        Err(e) => {
            let error = AppResponse::DatabaseError(format!("Error deleting record: {:?}", e));
            response_to_c_string(&error)
        }
    }
}

#[no_mangle]
pub extern "C" fn clear_all_records(db_state: *mut AppDbState) -> *const c_char {
    // Verificar que el puntero no sea nulo
    if db_state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to clear_all_records".to_string());
        return response_to_c_string(&error);
    }

    // Acceder al estado de la base de datos de manera segura
    let db_state = unsafe { &*db_state };

    // Llamar a la implementación de clear_all_records
    match db_state.clear_all_records() {
        Ok(_) => {
            let success = AppResponse::Ok("All records cleared successfully".to_string());
            response_to_c_string(&success)
        },
        Err(e) => {
            let error = AppResponse::DatabaseError(format!("Error clearing records: {:?}", e));
            response_to_c_string(&error)
        }
    }
}
#[no_mangle]
pub extern "C" fn reset_database(db_state: *mut AppDbState, name_ptr: *const c_char) -> *const c_char {
    // Verificar que los punteros no sean nulos
    if db_state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to reset_database".to_string());
        return response_to_c_string(&error);
    }

    if name_ptr.is_null() {
        let error = AppResponse::BadRequest("Null name pointer passed to reset_database".to_string());
        return response_to_c_string(&error);
    }

    // Convertir el puntero de nombre a un string de Rust
    let name = match c_ptr_to_string(name_ptr, "name") {
        Ok(name) => name,
        Err(error_ptr) => return error_ptr,
    };


    // Acceder al estado de la base de datos
    let db_state = unsafe { &mut *db_state };

    // Llamar a la implementación de reset_database
    match db_state.reset_database(&name) {
        Ok(_) => {
            let success = AppResponse::Ok(format!("Database '{}' was reset successfully", name));
            response_to_c_string(&success)
        },
        Err(e) => {
            let error = AppResponse::DatabaseError(format!("Error resetting database: {:?}", e));
            response_to_c_string(&error)
        }
    }
}


// Función auxiliar para convertir AppResponse a C string
fn response_to_c_string(response: &AppResponse) -> *const c_char {
    let json = match serde_json::to_string(response) {
        Ok(j) => j,
        Err(e) => {
            warn!("Error serializing response: {}", e);
            return std::ptr::null();
        }
    };

    match CString::new(json) {
        Ok(c_str) => c_str.into_raw(),
        Err(e) => {
            warn!("Error creating CString: {}", e);
            std::ptr::null()
        }
    }
}


/// Convierte un puntero C a una cadena Rust, manejando todos los posibles errores
///
/// Parámetros:
/// - `ptr`: puntero C a la cadena
/// - `field_name`: nombre del campo para mensajes de error más descriptivos
///
/// Retorna:
/// - `Ok(String)`: si la conversión fue exitosa
/// - `Err(*const c_char)`: puntero a un mensaje de error en formato C si falló
fn c_ptr_to_string(ptr: *const c_char, field_name: &str) -> Result<String, *const c_char> {
    // Verificar si el puntero es nulo
    if ptr.is_null() {
        let error = AppResponse::BadRequest(format!("Null {} pointer", field_name));
        return Err(response_to_c_string(&error));
    }

    // Convertir a str de Rust
    match unsafe { CStr::from_ptr(ptr).to_str() } {
        Ok(s) => Ok(s.to_string()),
        Err(e) => {
            let error = AppResponse::BadRequest(format!("Invalid UTF-8 in {}: ", e));
            Err(response_to_c_string(&error))
        }
    }
}