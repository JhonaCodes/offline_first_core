pub mod local_db_model;
pub mod local_db_state;

use crate::local_db_model::LocalDbModel;
use crate::local_db_state::AppDbState;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

#[no_mangle]
pub extern "C" fn create_db(name: *const c_char) -> *mut AppDbState {
    let name_str = unsafe { CStr::from_ptr(name).to_str().unwrap() };

    // Usar una ruta absoluta o relativa consistente
    let db_path = format!("./{}", name_str);
    println!("Rust: Creating/Opening database at: {}", db_path);

    let state = AppDbState::init(db_path);
    println!("Rust: Database initialized");

    Box::into_raw(Box::new(state))
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
    let state = unsafe { &*state };
    let id_str = unsafe { CStr::from_ptr(id).to_str().unwrap() };

    match state.get_by_id(id_str) {
        Ok(Some(model)) => {
            let json = serde_json::to_string(&model).unwrap();
            CString::new(json).unwrap().into_raw()
        },
        _ => std::ptr::null()
    }
}

#[no_mangle]
pub extern "C" fn get_all(state: *mut AppDbState) -> *const c_char {
    let state = unsafe { &*state };
    println!("Rust: Starting get_all"); // Debug

    match state.get() {
        Ok(models) => {
            println!("Rust: Found {} models", models.len()); // Debug
            let json = serde_json::to_string(&models).unwrap();
            println!("Rust: JSON to send: {}", json); // Debug
            CString::new(json).unwrap().into_raw()
        },
        Err(e) => {
            println!("Rust: Error in get_all: {:?}", e); // Debug
            std::ptr::null()
        }
    }
}


#[no_mangle]
pub extern "C" fn update_data(state: *mut AppDbState, json_ptr: *const c_char) -> *const c_char {
    let state = unsafe { &*state };
    let json_str = unsafe { CStr::from_ptr(json_ptr).to_str().unwrap() };
    let model: LocalDbModel = serde_json::from_str(json_str).unwrap();

    match state.update(model) {
        Ok(Some(updated_model)) => {
            let json = serde_json::to_string(&updated_model).unwrap();
            CString::new(json).unwrap().into_raw()
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_by_id() {
        // Inicializar la DB
        let state = AppDbState::init("test_db".to_string());

        // Crear y guardar un modelo de prueba
        let mut test_model = LocalDbModel {
            id: "1".to_string(),
            hash: "test_hash".to_string(),
            data: serde_json::json!({"test": "data"})
        };
        
        // Insertar el modelo
        state.push(test_model).unwrap();
        
        // 
        let get_all_data = state.get().unwrap();
        println!("{:?}",get_all_data);
        assert!(get_all_data.first().is_some());

        //Probar get_by_id
        let result = state.get_by_id("1").unwrap();
        assert!(result.is_some());
        
        let found_model = result.unwrap();
        assert_eq!(found_model.id, "1");
        
        // Probar ID que no existe
        let no_result = state.get_by_id("999").unwrap();
        assert!(no_result.is_none());

        test_model = LocalDbModel {
            id: "1".to_string(),
            hash: "test_hash".to_string(),
            data: serde_json::json!({"test": "datatyt6"})
        };
        
        let update_element = state.update(test_model).unwrap();
        match update_element {
            None => {
                println!("No se encontro elemento");
            }
            Some(element) => {
                println!("Elemetno actualzido {:?}", element);
            }
        }


        match state.delete_by_id("1") {
            Ok(was_deleted) => {
                println!("usuario eliminado {}", was_deleted)
            }
            Err(delted_error) => {
                println!("Error on delete element {:?}", delted_error);
            }
        }
    }
}