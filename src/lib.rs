pub mod local_db_model;
pub mod local_db_state;
mod test;
mod app_response;

use crate::local_db_model::LocalDbModel;
use crate::local_db_state::AppDbState;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use log::{warn, info};
use std::collections::HashMap;
use std::sync::{Mutex, atomic::{AtomicU64, Ordering}};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app_response::AppResponse;

// Global registry para tracking de AppDbState instances con generaciones
// para manejar hot reload scenarios
#[derive(Clone, Debug)]
struct InstanceInfo {
    generation: u64,
    created_at: u64,
    last_used: u64,
    is_valid: bool,
}

lazy_static::lazy_static! {
    static ref DB_INSTANCES: Mutex<HashMap<usize, InstanceInfo>> = Mutex::new(HashMap::new());
    static ref GENERATION_COUNTER: AtomicU64 = AtomicU64::new(1);
}
#[no_mangle]
pub extern "C" fn create_db(name: *const c_char) -> *mut AppDbState {
    let name_str = unsafe { CStr::from_ptr(name).to_str().unwrap() };

    // Usar una ruta absoluta o relativa consistente
    let db_path = format!("./{}", name_str);

    let state = AppDbState::init(db_path);
    info!("Rust: Database initialized");
    
    match state {
        Ok(response) => {
            let boxed = Box::new(response);
            let ptr = Box::into_raw(boxed);
            
            // Register the instance with generation tracking
            let addr = ptr as usize;
            let generation = GENERATION_COUNTER.fetch_add(1, Ordering::SeqCst);
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
            
            if let Ok(mut instances) = DB_INSTANCES.lock() {
                instances.insert(addr, InstanceInfo {
                    generation,
                    created_at: now,
                    last_used: now,
                    is_valid: true,
                });
                info!("Registered DB instance: {} with generation: {}", addr, generation);
            }
            
            ptr
        }
        Err(e) => {
            warn!("Failed to create database: {:?}", e);
            std::ptr::null_mut()
        }
    }
}


#[no_mangle]
pub extern "C" fn push_data(state: *mut AppDbState, json_ptr: *const c_char) -> *const c_char {
    // Validate instance before proceeding
    if !validate_and_update_instance(state) {
        let error = AppResponse::BadRequest("Invalid or stale database instance".to_string());
        return response_to_c_string(&error);
    }

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
    // Validate instance before proceeding
    if !validate_and_update_instance(state) {
        let error = AppResponse::BadRequest("Invalid or stale database instance".to_string());
        return response_to_c_string(&error);
    }

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
    // Validate instance before proceeding
    if !validate_and_update_instance(state) {
        let error = AppResponse::BadRequest("Invalid or stale database instance".to_string());
        return response_to_c_string(&error);
    }

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
    // Validate instance before proceeding
    if !validate_and_update_instance(state) {
        let error = AppResponse::BadRequest("Invalid or stale database instance".to_string());
        return response_to_c_string(&error);
    }

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
    // Validate instance before proceeding
    if !validate_and_update_instance(db_state) {
        let error = AppResponse::BadRequest("Invalid or stale database instance".to_string());
        return response_to_c_string(&error);
    }

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
    // Validate instance before proceeding
    if !validate_and_update_instance(db_state) {
        let error = AppResponse::BadRequest("Invalid or stale database instance".to_string());
        return response_to_c_string(&error);
    }

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
    // Validate instance before proceeding
    if !validate_and_update_instance(db_state) {
        let error = AppResponse::BadRequest("Invalid or stale database instance".to_string());
        return response_to_c_string(&error);
    }

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

/// Properly closes a database connection and frees all associated resources
/// This function should be called before hot restart or app termination
#[no_mangle]
pub extern "C" fn close_database(db_state: *mut AppDbState) -> *const c_char {
    if db_state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to close_database".to_string());
        return response_to_c_string(&error);
    }

    let addr = db_state as usize;
    
    // Check if the instance is registered and valid
    let is_valid = if let Ok(instances) = DB_INSTANCES.lock() {
        if let Some(info) = instances.get(&addr) {
            info.is_valid
        } else {
            false
        }
    } else {
        false
    };

    if !is_valid {
        let error = AppResponse::BadRequest("Invalid or already closed database instance".to_string());
        return response_to_c_string(&error);
    }

    // Safely drop the database instance
    unsafe {
        let _db_box = Box::from_raw(db_state);
        // _db_box will be automatically dropped here, calling AppDbState's Drop implementation
    }

    // Remove from registry
    if let Ok(mut instances) = DB_INSTANCES.lock() {
        instances.remove(&addr);
        info!("Unregistered DB instance: {}", addr);
    }

    let success = AppResponse::Ok("Database closed successfully".to_string());
    response_to_c_string(&success)
}

/// Frees memory allocated for C string responses
/// Should be called after consuming the response from any FFI function
#[no_mangle]
pub extern "C" fn free_c_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    
    unsafe {
        let _ = CString::from_raw(ptr);
        // CString will be automatically dropped here, freeing the memory
    }
}

/// Validates that a database pointer is still valid and registered
/// Also updates last_used timestamp for the instance
#[no_mangle]
pub extern "C" fn is_database_valid(db_state: *mut AppDbState) -> bool {
    if db_state.is_null() {
        return false;
    }

    let addr = db_state as usize;
    if let Ok(mut instances) = DB_INSTANCES.lock() {
        if let Some(info) = instances.get_mut(&addr) {
            if info.is_valid {
                // Update last_used timestamp
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                info.last_used = now;
                true
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    }
}

/// Validates that a database instance is from the expected generation
/// This helps detect hot reload scenarios where instances become stale
#[no_mangle]
pub extern "C" fn validate_instance_generation(
    db_state: *mut AppDbState, 
    expected_generation: u64
) -> bool {
    if db_state.is_null() {
        return false;
    }

    let addr = db_state as usize;
    if let Ok(instances) = DB_INSTANCES.lock() {
        if let Some(info) = instances.get(&addr) {
            info.is_valid && info.generation == expected_generation
        } else {
            false
        }
    } else {
        false
    }
}

/// Ping function for heartbeat monitoring
/// Returns the current generation and status of the instance
#[no_mangle]
pub extern "C" fn ping_database(db_state: *mut AppDbState) -> *const c_char {
    if db_state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to ping_database".to_string());
        return response_to_c_string(&error);
    }

    let addr = db_state as usize;
    if let Ok(mut instances) = DB_INSTANCES.lock() {
        if let Some(info) = instances.get_mut(&addr) {
            if info.is_valid {
                // Update last_used timestamp
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                info.last_used = now;
                
                let ping_response = format!(
                    "{{\"generation\": {}, \"created_at\": {}, \"last_used\": {}, \"is_valid\": true}}", 
                    info.generation, info.created_at, info.last_used
                );
                let success = AppResponse::Ok(ping_response);
                response_to_c_string(&success)
            } else {
                let error = AppResponse::BadRequest("Instance is marked as invalid".to_string());
                response_to_c_string(&error)
            }
        } else {
            let error = AppResponse::NotFound("Instance not found in registry".to_string());
            response_to_c_string(&error)
        }
    } else {
        let error = AppResponse::DatabaseError("Failed to access instance registry".to_string());
        response_to_c_string(&error)
    }
}

/// Cleanup function to remove stale instances
/// Should be called periodically to clean up old instances
#[no_mangle]
pub extern "C" fn cleanup_stale_instances() -> *const c_char {
    let cleanup_threshold = 300; // 5 minutes in seconds
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    
    if let Ok(mut instances) = DB_INSTANCES.lock() {
        let initial_count = instances.len();
        instances.retain(|_addr, info| {
            // Keep instance if it's valid and recently used, or still young
            info.is_valid && (now - info.last_used < cleanup_threshold || now - info.created_at < 60)
        });
        let final_count = instances.len();
        let cleaned = initial_count - final_count;
        
        info!("Cleaned up {} stale instances. {} instances remain.", cleaned, final_count);
        
        let success = AppResponse::Ok(format!("Cleaned {} stale instances", cleaned));
        response_to_c_string(&success)
    } else {
        let error = AppResponse::DatabaseError("Failed to access instance registry for cleanup".to_string());
        response_to_c_string(&error)
    }
}

/// Get the current generation counter value
#[no_mangle]
pub extern "C" fn get_current_generation() -> u64 {
    GENERATION_COUNTER.load(Ordering::SeqCst)
}

/// Mark an instance as invalid (useful for hot reload scenarios)
#[no_mangle]
pub extern "C" fn invalidate_instance(db_state: *mut AppDbState) -> bool {
    if db_state.is_null() {
        return false;
    }

    let addr = db_state as usize;
    if let Ok(mut instances) = DB_INSTANCES.lock() {
        if let Some(info) = instances.get_mut(&addr) {
            info.is_valid = false;
            info!("Invalidated instance: {}", addr);
            true
        } else {
            false
        }
    } else {
        false
    }
}

// Función auxiliar para validar instancia y actualizar timestamp
fn validate_and_update_instance(db_state: *mut AppDbState) -> bool {
    if db_state.is_null() {
        return false;
    }

    let addr = db_state as usize;
    if let Ok(mut instances) = DB_INSTANCES.lock() {
        if let Some(info) = instances.get_mut(&addr) {
            if info.is_valid {
                // Update last_used timestamp
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                info.last_used = now;
                true
            } else {
                warn!("Attempt to use invalid instance: {}", addr);
                false
            }
        } else {
            warn!("Attempt to use unregistered instance: {}", addr);
            false
        }
    } else {
        warn!("Failed to access instance registry");
        false
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