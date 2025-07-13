#[cfg(test)]
pub mod tests {
    use std::path::Path;
    use crate::local_db_model::LocalDbModel;
    use crate::local_db_state::AppDbState;
    use std::time::{SystemTime, UNIX_EPOCH};

    // Helper function for safe timestamp generation in tests
    fn safe_test_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(1640995200) // Fallback to 2022-01-01 timestamp
    }
    use log::{info, warn};
    use redb::DatabaseError;

    // Función helper para crear modelos de prueba (updated for String data)
    fn create_test_model(id: &str, data: Option<serde_json::Value>) -> LocalDbModel {
        let data_value = data.unwrap_or(serde_json::json!({"test": "data"}));
        let data_string = serde_json::to_string(&data_value)
            .unwrap_or_else(|e| {
                eprintln!("Failed to serialize test data: {}", e);
                r#"{"test":"fallback_data"}"#.to_string()
            });
        LocalDbModel {
            id: id.to_string(),
            hash: format!("hash_{}", id),
            data: data_string,
        }
    }
    
    // Helper function to create model with string data directly
    fn create_test_model_with_string(id: &str, data_json: &str) -> LocalDbModel {
        LocalDbModel {
            id: id.to_string(),
            hash: format!("hash_{}", id),
            data: data_json.to_string(),
        }
    }


    fn cleanup_test_databases() {

        if let Ok(_entries) = std::fs::read_dir(".") {

            // Intentar leer el directorio
            match std::fs::read_dir(".") {
                Ok(entries) => {
                    for entry_result in entries {
                        // Manejar cada entrada de forma segura
                        let entry = match entry_result {
                            Ok(e) => e,
                            Err(e) => {
                                info!("Error al leer entrada del directorio: {}", e);
                                continue;
                            }
                        };

                        // Manejar la conversión del nombre de archivo a String
                        let file_name = match entry.file_name().into_string() {
                            Ok(name) => name,
                            Err(_) => {
                                warn!("Error: nombre de archivo contiene caracteres inválidos");
                                continue;
                            }
                        };

                        // Procesar solo las bases de datos de prueba
                        if !file_name.starts_with("database_tested_") {
                            continue;
                        }

                        // Intentar eliminar el archivo
                        match std::fs::remove_file(entry.path()) {
                            Ok(_) => info!("Base de datos eliminada: {}", file_name),
                            Err(e) => warn!("Error eliminando {}: {}", file_name, e),
                        }
                    }
                },
                Err(e) => {
                    warn!("Error al leer el directorio: {}", e);
                }
            }
        }
        
    }

    fn generate_unique_db_name(prefix: &str) -> String {
        format!("database_tested_{}_{}",
                prefix,
                safe_test_timestamp()
        )
    }
    
    // Tests unitarios para funciones individuales
    #[test]
    fn test_push() {
        let state = AppDbState::init(generate_unique_db_name("push"));
        let model = create_test_model("1", None);
        match state {
            Ok(results) => {
                let result = results.push(model.clone()).unwrap();

                assert_eq!(result.id, model.id);
                assert_eq!(result.hash, model.hash);

                // Verificar que se guardó correctamente
                let stored = results.get_by_id("1").unwrap().unwrap();
                assert_eq!(stored.id, model.id);
            }
            Err(_) => {
                warn!("Error on testing")
            }
        }
    }
    
    #[test]
    fn test_db_already_open() {
        // Use a fixed database name to ensure we're testing the same database
        let db_name = "database_tested_already_open";

        // Clean up: Remove the database if it exists from previous test runs
        let path = Path::new(db_name);
        if path.exists() {
            std::fs::remove_file(path).expect("Failed to remove existing test database");
        }

        // First instance - should create the database
        let first_instance = AppDbState::init(db_name.to_string());
        assert!(first_instance.is_ok(), "First instance should be created successfully");
        let first_db = first_instance.unwrap();

        info!("First instance opened successfully");

        // Second instance - attempt to open the same database while first is still open
        let second_instance = AppDbState::init(db_name.to_string());

        // Check if we were able to open a second instance
        if second_instance.is_ok() {
            info!("Second instance opened successfully - database supports multiple connections");

            // Test writing to the first instance
            let model_1 = create_test_model("test1", None);
            let result_1 = first_db.push(model_1.clone());
            info!("Write to first instance: {}", result_1.is_ok());

            // Test writing to the second instance
            let second_db = second_instance.as_ref().unwrap();
            let model_2 = create_test_model("test2", None);
            let result_2 = second_db.push(model_2.clone());
            info!("Write to second instance: {}", result_2.is_ok());

            // Test cross-instance data visibility (if each instance can read data written by the other)
            if result_1.is_ok() && result_2.is_ok() {
                // Try to read from first instance what was written by second
                let read_1 = first_db.get_by_id("test2");
                info!("First instance can read data from second: {}",
                         read_1.is_ok() && read_1.unwrap().is_some());

                // Try to read from second instance what was written by first
                let read_2 = second_db.get_by_id("test1");
                info!("Second instance can read data from first: {}",
                         read_2.is_ok() && read_2.unwrap().is_some());
            }
        } else {
            info!("Second instance failed to open the same database");

            // Analyze the specific error type
            match second_instance.err().unwrap() {
                DatabaseError::Storage(storage_err) => {
                    info!("Storage error: {:?}", storage_err);
                },
                _ => {
                    info!("Other type of error");
                }
            }

            // Verify that the first instance still works
            let model = create_test_model("test1", None);
            let result = first_db.push(model);
            info!("First instance still functioning: {}", result.is_ok());
        }

        // Clean up: Remove the test database
        if path.exists() {
            std::fs::remove_file(path).expect("Failed to clean up test database");
        }
    }

    #[test]
    fn test_get_by_id() {
        let state = AppDbState::init(generate_unique_db_name("get"));

        match state {
            Ok(response) => {
                // Probar con ID que no existe
                let no_result = response.get_by_id("nonexistent").unwrap();
                assert!(no_result.is_none());

                // Probar con ID existente
                let model = create_test_model("1", None);
                response.push(model.clone()).unwrap();

                let result = response.get_by_id("1").unwrap();
                assert!(result.is_some());
                assert_eq!(result.unwrap().id, "1");
            }
            Err(_) => {
                warn!("Error on get data")
            }
        }
    }
    #[test]
    fn test_get_all() {
        // Crear nombre único para la base de datos usando timestamp
        let db_name = generate_unique_db_name("get_all");

        // Inicializar con una nueva base de datos
        match AppDbState::init(db_name.clone()) {
            Ok(mut state) => {
                // Asegurarnos de que empezamos con una base de datos limpia
                state.reset_database(&db_name).unwrap();

                // Ahora sí verificar que está vacía
                let empty_results = state.get().unwrap();
                assert!(empty_results.is_empty(), "La base de datos debería estar vacía inicialmente");

                // Insertar primer registro y verificar
                let model1 = create_test_model("1", None);
                state.push(model1).unwrap();

                let results = state.get().unwrap();
                assert_eq!(results.len(), 1, "Debería haber exactamente 1 registro");

                // Insertar segundo registro y verificar
                let model2 = create_test_model("2", None);
                state.push(model2).unwrap();

                let results = state.get().unwrap();
                assert_eq!(results.len(), 2, "Debería haber exactamente 2 registros");

                // Insertar tercer registro y verificar
                let model3 = create_test_model("3", None);
                state.push(model3).unwrap();

                let results = state.get().unwrap();
                assert_eq!(results.len(), 3, "Debería haber exactamente 3 registros");

                // Verificar que podemos obtener cada registro individualmente
                assert!(state.get_by_id("1").unwrap().is_some());
                assert!(state.get_by_id("2").unwrap().is_some());
                assert!(state.get_by_id("3").unwrap().is_some());
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_get_all");
            }
        }
    }
    #[test]
    fn test_update() {
        match AppDbState::init(generate_unique_db_name("update")) {
            Ok(state) => {
                // Intentar actualizar un registro que no existe
                let non_existent = create_test_model("999", None);
                let update_result = state.update(non_existent).unwrap();
                assert!(update_result.is_none());

                // Actualizar un registro existente
                let model = create_test_model("1", Some(serde_json::json!({"original": true})));
                state.push(model).unwrap();

                let updated_model = create_test_model("1", Some(serde_json::json!({"updated": true})));
                let result = state.update(updated_model.clone()).unwrap();

                assert!(result.is_some());
                let updated = state.get_by_id("1").unwrap().unwrap();
                assert_eq!(updated.data, updated_model.data);
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_update");
            }
        }
    }
    #[test]
    fn test_delete() {
        match AppDbState::init(generate_unique_db_name("delete")) {
            Ok(state) => {
                // Intentar eliminar un registro que no existe
                let delete_result = state.delete_by_id("nonexistent").unwrap();
                assert!(!delete_result);

                // Eliminar un registro existente
                let model = create_test_model("1", None);
                state.push(model).unwrap();

                let delete_result = state.delete_by_id("1").unwrap();
                assert!(delete_result);

                let not_found = state.get_by_id("1").unwrap();
                assert!(not_found.is_none());
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_delete");
            }
        }
    }
    #[test]
    fn test_clear_all_records() {
        match AppDbState::init(generate_unique_db_name("clear")) {
            Ok(state) => {
                // Limpiar DB vacía
                let count = state.clear_all_records().unwrap();
                assert_eq!(count, 0);

                // Limpiar DB con registros
                for i in 1..=3 {
                    state.push(create_test_model(&i.to_string(), None)).unwrap();
                }

                let count = state.clear_all_records().unwrap();
                assert_eq!(count, 3);

                let remaining = state.get().unwrap();
                assert!(remaining.is_empty());
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_clear_all_records");
            }
        }
    }
    #[test]
    fn test_reset_database() {
        match AppDbState::init(generate_unique_db_name("reset")) {
            Ok(mut state) => {
                // Añadir algunos registros
                for i in 1..=3 {
                    state.push(create_test_model(&i.to_string(), None)).unwrap();
                }

                let new_name = generate_unique_db_name("hard_reset");

                let reset = state.reset_database(&new_name).unwrap();
                assert!(reset);

                let empty = state.get().unwrap();
                assert!(empty.is_empty());
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_reset_database");
            }
        }
    }
    #[test]
    fn test_basic_operations() {
        match AppDbState::init(generate_unique_db_name("basic")) {
            Ok(state) => {
                // Insertar varios registros en secuencia
                for i in 1..=5 {
                    let model = create_test_model(&i.to_string(), None);
                    let result = state.push(model).unwrap();
                    assert_eq!(result.id, i.to_string());
                }

                // Verificar que todos los registros se insertaron correctamente
                let all_records = state.get().unwrap();
                assert_eq!(all_records.len(), 5, "Deberían haberse insertado 5 registros");

                // Verificar que cada registro existe y tiene los datos correctos
                for i in 1..=5 {
                    let record = state.get_by_id(&i.to_string()).unwrap();
                    assert!(record.is_some(), "El registro {} debería existir", i);
                    let record = record.unwrap();
                    assert_eq!(record.hash, format!("hash_{}", i));
                }
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_basic_operations");
            }
        }
    }
    #[test]
    fn test_large_dataset() {
        match AppDbState::init(generate_unique_db_name("large_data")) {
            Ok(state) => {
                // Insertar un conjunto grande de datos
                for i in 1..=100 {
                    let model = create_test_model(
                        &i.to_string(),
                        Some(serde_json::json!({
                    "name": format!("test_{}", i),
                    "value": i,
                    "data": vec![1, 2, 3, 4, 5]
                }))
                    );
                    state.push(model).unwrap();
                }

                // Verificar cantidad total
                let all_records = state.get().unwrap();
                assert_eq!(all_records.len(), 100);

                // Verificar rendimiento de búsqueda
                let start = std::time::Instant::now();
                let _result = state.get_by_id("50").unwrap();
                let duration = start.elapsed();
                assert!(duration.as_millis() < 100, "La búsqueda tardó demasiado");
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_large_dataset");
            }
        }
    }
    #[test]
    fn test_data_integrity() {
        match AppDbState::init(generate_unique_db_name("integrity")) {
            Ok(state) => {
                // Probar con datos más complejos
                let complex_data = serde_json::json!({
            "nested": {
                "array": [1, 2, 3],
                "object": {
                    "key": "value",
                    "number": 42,
                    "boolean": true,
                    "null": null
                }
            },
            "special_chars": "!@#$%^&*()_+-=[]{}|;:'\",.<>?/\\",
            "unicode": "Hello, 世界! 🌍"
            });

                let model = create_test_model("complex", Some(complex_data.clone()));
                state.push(model).unwrap();

                // Verificar que los datos se mantienen intactos (now as JSON string)
                let retrieved = state.get_by_id("complex").unwrap().unwrap();
                let expected_json = serde_json::to_string(&complex_data)
                    .unwrap_or_else(|e| {
                        eprintln!("Failed to serialize complex_data for comparison: {}", e);
                        "{}".to_string()
                    });
                assert_eq!(retrieved.data, expected_json);
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_data_integrity");
            }
        }
    }
    #[test]
    fn test_edge_cases() {
        match AppDbState::init(generate_unique_db_name("edge_cases")) {
            Ok(state) => {
                // Probar con ID vacío
                let empty_id_model = create_test_model("", None);
                state.push(empty_id_model).unwrap();
                assert!(state.get_by_id("").unwrap().is_some());

                // Probar con datos muy grandes
                let large_data = serde_json::json!({
            "large_array": vec![0; 10000],
            "large_string": "a".repeat(10000)
            });
                let large_model = create_test_model("large", Some(large_data));
                state.push(large_model).unwrap();

                // Probar actualización con datos diferentes
                let updated_model = create_test_model("large", Some(serde_json::json!({"small": "data"})));
                state.update(updated_model).unwrap();
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_edge_cases");
            }
        }
    }
    #[test]
    fn test_edge_cases_extended() {
        match AppDbState::init(generate_unique_db_name("edge_cases_extended")) {
            Ok(state) => {
                // 1. IDs con caracteres especiales
                let special_id_model = create_test_model("!@#$%^&*()", None);
                state.push(special_id_model).unwrap();
                assert!(state.get_by_id("!@#$%^&*()").unwrap().is_some());

                // 2. Datos nulos o vacíos
                let null_model = create_test_model("null_data", Some(serde_json::json!(null)));
                state.push(null_model).unwrap();

                let empty_model = create_test_model("empty_data", Some(serde_json::json!({})));
                state.push(empty_model).unwrap();

                // 3. Valores numéricos extremos
                let extreme_values = create_test_model("extreme", Some(serde_json::json!({
            "max_i64": i64::MAX,
            "min_i64": i64::MIN,
            "max_f64": f64::MAX,
            "min_f64": f64::MIN
            })));
                state.push(extreme_values).unwrap();

                // 4. Caracteres Unicode y emojis en datos
                let unicode_model = create_test_model("unicode", Some(serde_json::json!({
            "text": "Hello 世界 🌍 👋 🤖"
            })));
                state.push(unicode_model).unwrap();

                // 5. Arrays anidados profundos
                let nested_array = create_test_model("nested", Some(serde_json::json!([
            [[[[[1,2,3]]]]]
            ])));
                state.push(nested_array).unwrap();

                // 6. Actualización repetitiva del mismo registro
                let repeated_model = create_test_model("repeated", None);
                state.push(repeated_model.clone()).unwrap();

                for i in 1..100 {
                    let updated = create_test_model("repeated", Some(serde_json::json!({
                "update_number": i
                })));
                    state.update(updated).unwrap();
                }

                // 7. IDs muy largos
                let long_id_model = create_test_model(&"a".repeat(1000), None);
                state.push(long_id_model).unwrap();

                // 8. Operaciones rápidas consecutivas
                for i in 1..100 {
                    let quick_model = create_test_model(&format!("quick_{}", i), None);
                    state.push(quick_model).unwrap();
                    state.get_by_id(&format!("quick_{}", i)).unwrap();
                    state.delete_by_id(&format!("quick_{}", i)).unwrap();
                }
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_edge_cases_extended");
            }
        }
    }
    #[test]
    fn test_full_workflow() {
        match AppDbState::init(generate_unique_db_name("workflow")) {
            Ok(mut state) => {
                // 1. Crear y guardar modelo inicial
                let test_model = create_test_model("1", Some(serde_json::json!({"test": "data"})));
                state.push(test_model).unwrap();

                // Esperar un momento para asegurar que la escritura se completó
                std::thread::sleep(std::time::Duration::from_millis(100));

                // 2. Verificar get_all
                let get_all_data = state.get().unwrap();
                assert!(!get_all_data.is_empty(), "La base de datos no debería estar vacía");
                assert_eq!(get_all_data.len(), 1, "Debería haber exactamente un registro");

                // 3. Verificar get_by_id
                let result = state.get_by_id("1").unwrap();
                assert!(result.is_some(), "Debería encontrar el registro con id 1");
                assert_eq!(result.unwrap().id, "1");

                // 4. Actualizar modelo
                let updated_model = create_test_model("1", Some(serde_json::json!({"test": "updated_data"})));
                let update_result = state.update(updated_model).unwrap();
                assert!(update_result.is_some());

                std::thread::sleep(std::time::Duration::from_millis(100));

                // 5. Verificar la actualización
                let updated = state.get_by_id("1").unwrap().unwrap();
                assert_eq!(updated.data, r#"{"test":"updated_data"}"#);

                // 6. Probar delete
                assert!(state.delete_by_id("1").unwrap());

                std::thread::sleep(std::time::Duration::from_millis(100));

                assert!(state.get_by_id("1").unwrap().is_none());

                // 7. Probar clear_all_records con múltiples registros
                for i in 1..=3 {
                    let model = create_test_model(&i.to_string(), None);
                    state.push(model).unwrap();
                    // Verificar después de cada inserción
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    assert!(state.get_by_id(&i.to_string()).unwrap().is_some());
                }

                let cleared = state.clear_all_records().unwrap();
                assert_eq!(cleared, 3);

                std::thread::sleep(std::time::Duration::from_millis(100));

                // 8. Verificar que está vacío después de clear
                let after_clear = state.get().unwrap();
                assert!(after_clear.is_empty(), "La base de datos debería estar vacía después de clear");

                // 9. Reset de la base de datos
                let new_db_name = format!(
                    "database_tested_{}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                );

                let reset_result = state.reset_database(&new_db_name);
                assert!(reset_result.is_ok());

                std::thread::sleep(std::time::Duration::from_millis(100));

                // 10. Verificar que está vacío después del reset
                let final_check = state.get().unwrap();
                assert!(final_check.is_empty(), "La base de datos debería estar vacía después del reset");
                cleanup_test_databases();
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_full_workflow");
            }
        }
    }
    #[test]
    fn test_error_handling() {
        match AppDbState::init(generate_unique_db_name("handling")) {
            Ok(state) => {
                // Probar operaciones con IDs no existentes
                assert!(state.get_by_id("nonexistent").unwrap().is_none());
                assert!(!state.delete_by_id("nonexistent").unwrap());
                assert!(state.update(create_test_model("nonexistent", None)).unwrap().is_none());

                // Probar operaciones después de limpiar la DB
                state.push(create_test_model("1", None)).unwrap();
                state.clear_all_records().unwrap();
                assert!(state.get_by_id("1").unwrap().is_none());
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_error_handling");
            }
        }
    }
    #[test]
    fn test_interrupted_operations() {
        match AppDbState::init(generate_unique_db_name("interrupted")) {
            Ok(state) => {
                // Simular una operación que podría interrumpirse
                let model = create_test_model("1", None);
                state.push(model).unwrap();

                // Intentar actualizar y eliminar el mismo registro "simultáneamente"
                let updated_model = create_test_model("1", Some(serde_json::json!({"updated": true})));
                state.update(updated_model).unwrap();
                state.delete_by_id("1").unwrap();

                // Verificar el estado final
                assert!(state.get_by_id("1").unwrap().is_none());
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_interrupted_operations");
            }
        }
    }
    #[test]
    fn test_recovery_after_errors() {
        match AppDbState::init(generate_unique_db_name("recovery")) {
            Ok(state) => {
                // Operación exitosa
                let model = create_test_model("1", None);
                state.push(model).unwrap();

                // Intentar operaciones que deberían fallar
                let result = state.get_by_id("nonexistent");
                assert!(result.is_ok()); // Debería manejar el error gracefully

                // Verificar que podemos seguir operando después del error
                let model2 = create_test_model("2", None);
                assert!(state.push(model2).is_ok());
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_recovery_after_errors");
            }
        }
    }
    #[test]
    fn test_data_validation() {
        match AppDbState::init(generate_unique_db_name("validation")) {
            Ok(state) => {
                // Probar con diferentes tipos de datos
                let models = vec![
                    create_test_model("bool", Some(serde_json::json!(true))),
                    create_test_model("number", Some(serde_json::json!(42.5))),
                    create_test_model("array", Some(serde_json::json!([1,2,3]))),
                    create_test_model("nested", Some(serde_json::json!({
                "a": {"b": {"c": 1}}
                }))),
                ];

                for model in models {
                    state.push(model).unwrap();
                }

                // Verificar que el dato se almacena como string
                let retrieved = state.get_by_id("number").unwrap().unwrap();
                // Data is now stored as JSON string - just verify it's not empty
                assert!(!retrieved.data.is_empty());
                assert!(retrieved.data.contains("42"));
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_data_validation");
            }
        }
    }
    #[test]
    fn test_batch_operations() {
        match AppDbState::init(generate_unique_db_name("batch")) {
            Ok(state) => {
                // Insertar múltiples registros
                let models: Vec<_> = (1..100)
                    .map(|i| create_test_model(&i.to_string(), None))
                    .collect();

                for model in models {
                    state.push(model).unwrap();
                }

                // Eliminar múltiples registros
                for i in 1..50 {
                    state.delete_by_id(&i.to_string()).unwrap();
                }

                // Verificar el estado final
                let remaining = state.get().unwrap();
                assert_eq!(remaining.len(), 50);
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_batch_operations");
            }
        }
    }
    #[test]
    fn test_data_consistency() {
        match AppDbState::init(generate_unique_db_name("consistency")) {
            Ok(state) => {
                // Crear registro inicial
                let original = create_test_model("1", Some(serde_json::json!({
            "count": 0,
            "timestamp": SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
            })));
                state.push(original).unwrap();

                // Realizar múltiples actualizaciones
                for i in 1..10 {
                    let updated = create_test_model("1", Some(serde_json::json!({
                "count": i,
                "timestamp": SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                })));
                    state.update(updated).unwrap();
                }

                // Verificar que los datos se actualizaron
                let final_state = state.get_by_id("1").unwrap().unwrap();
                // Data is stored as JSON string - just verify it contains the updated value
                assert!(!final_state.data.is_empty());
                assert!(final_state.data.contains("\"count\":9"));
            },
            Err(_) => {
                panic!("Error al inicializar la base de datos para test_data_consistency");
            }
        }
    }
}