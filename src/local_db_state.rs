use crate::local_db_model::LocalDbModel;
use log::{info, warn};
use lmdb::{Environment, Database, Transaction, WriteFlags, Cursor, DatabaseFlags, Error as LmdbError};
use std::fs;
use std::path::Path;
use crate::app_response::AppResponse;

const MAIN_DB_NAME: &str = "main";

pub struct AppDbState {
    env: Environment,
    db: Database,
    path: String, // Store the path for potential database reset
}

impl AppDbState {
    pub fn init(name: String) -> Result<Self, LmdbError> {
        // LMDB necesita un directorio, no un archivo
        let db_dir = format!("{}.lmdb", name);
        let path = Path::new(&db_dir);
        
        // Crear el directorio si no existe
        if !path.exists() {
            fs::create_dir_all(path).map_err(|_| LmdbError::Other(2))?;
        }
        
        // Crear o abrir el environment LMDB
        let env = Environment::new()
            .set_max_dbs(10)
            .set_map_size(1024 * 1024 * 1024) // 1GB
            .open(path)?;
        
        info!("LMDB environment opened at {}", name);
        
        // Abrir o crear la base de datos principal
        let db = env.create_db(Some(MAIN_DB_NAME), DatabaseFlags::empty())?;
        
        info!("Database initialized successfully");
        
        Ok(Self {
            env,
            db,
            path: db_dir
        })
    }

    pub fn push(&self, model: LocalDbModel) -> Result<LocalDbModel, AppResponse> {
        let json = serde_json::to_string(&model)?;
        
        let mut txn = self.env.begin_rw_txn().map_err(AppResponse::from)?;
        txn.put(self.db, &model.id, &json, WriteFlags::empty()).map_err(AppResponse::from)?;
        txn.commit().map_err(AppResponse::from)?;
        
        Ok(model)
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<LocalDbModel>, LmdbError> {
        let txn = self.env.begin_ro_txn()?;
        
        match txn.get(self.db, &id) {
            Ok(bytes) => {
                let json_str = std::str::from_utf8(bytes)
                    .map_err(|_| LmdbError::Other(1))?;
                let model = serde_json::from_str(json_str)
                    .map_err(|_| LmdbError::Other(1))?;
                Ok(Some(model))
            }
            Err(LmdbError::NotFound) => {
                info!("No value found for id {}", id);
                Ok(None)
            }
            Err(e) => Err(e)
        }
    }

    pub fn get(&self) -> Result<Vec<LocalDbModel>, LmdbError> {
        let mut models = Vec::new();
        
        let txn = self.env.begin_ro_txn()?;
        let mut cursor = txn.open_ro_cursor(self.db)?;
        
        for (_, value) in cursor.iter() {
            match std::str::from_utf8(value) {
                Ok(json_str) => {
                    match serde_json::from_str::<LocalDbModel>(json_str) {
                        Ok(model) => models.push(model),
                        Err(e) => info!("Error deserializing model: {:?}", e),
                    }
                }
                Err(e) => info!("Error converting to UTF-8: {:?}", e),
            }
        }
        
        Ok(models)
    }

    pub fn delete_by_id(&self, id: &str) -> Result<bool, LmdbError> {
        let mut txn = self.env.begin_rw_txn()?;
        
        // Verificar si existe antes de eliminar
        let existed = match txn.get(self.db, &id) {
            Ok(_) => true,
            Err(LmdbError::NotFound) => false,
            Err(e) => return Err(e),
        };
        
        if existed {
            txn.del(self.db, &id, None)?;
        }
        
        txn.commit()?;
        Ok(existed)
    }

    pub fn update(&self, model: LocalDbModel) -> Result<Option<LocalDbModel>, LmdbError> {
        let mut txn = self.env.begin_rw_txn()?;
        
        // Verificar si existe
        let exists = match txn.get(self.db, &model.id) {
            Ok(_) => true,
            Err(LmdbError::NotFound) => false,
            Err(e) => return Err(e),
        };
        
        if exists {
            let json = serde_json::to_string(&model)
                .map_err(|_| LmdbError::Other(1))?;
            txn.put(self.db, &model.id, &json, WriteFlags::empty())?;
            txn.commit()?;
            Ok(Some(model))
        } else {
            Ok(None)
        }
    }

    /// Deletes all records from the database while maintaining the database structure
    /// Returns the number of records deleted
    /// This is useful when you want to clear data but keep using the same database
    pub fn clear_all_records(&self) -> Result<usize, LmdbError> {
        let mut txn = self.env.begin_rw_txn()?;
        let mut count = 0;
        
        // Recopilar todas las claves primero
        let keys: Vec<Vec<u8>> = {
            let mut cursor = txn.open_ro_cursor(self.db)?;
            cursor.iter()
                .map(|(key, _)| key.to_vec())
                .collect()
        };
        
        for key in keys {
            match txn.del(self.db, &key, None) {
                Ok(_) => count += 1,
                Err(e) => warn!("Error deleting key: {:?}", e),
            }
        }
        txn.commit()?;
        Ok(count)
    }

    /// Completely resets the database by:
    /// 1. Closing the current connection
    /// 2. Deleting the database file
    /// 3. Creating a new database
    /// This is useful when you want to start completely fresh
    /// Returns true if successful
    pub fn reset_database(&mut self, name: &str) -> Result<bool, Box<dyn std::error::Error>> {
        // El environment actual se cerrará automáticamente cuando se reemplace
        
        // Eliminar el directorio de la base de datos
        if Path::new(&self.path).exists() {
            fs::remove_dir_all(&self.path)?;
        }
        
        // Crear nueva base de datos
        let new_db_dir = format!("{}.lmdb", name);
        let path = Path::new(&new_db_dir);
        
        // Crear el directorio si no existe
        if !path.exists() {
            fs::create_dir_all(path)?;
        }
        
        // Crear nuevo environment
        let new_env = Environment::new()
            .set_max_dbs(10)
            .set_map_size(1024 * 1024 * 1024)
            .open(path)?;
            
        // Abrir nueva base de datos
        let new_db = new_env.create_db(Some(MAIN_DB_NAME), DatabaseFlags::empty())?;
        
        // Actualizar referencias
        self.env = new_env;
        self.db = new_db;
        self.path = new_db_dir;
        
        Ok(true)
    }
    
    /// Cierra explícitamente la conexión a la base de datos
    /// Útil para liberar recursos antes de hot restart
    /// Nota: En LMDB, las conexiones se cierran automáticamente cuando el Environment se dropea
    /// Esta función sirve como indicador explícito de que la conexión ya no debe usarse
    pub fn close_database(&mut self) -> Result<(), LmdbError> {
        // En LMDB, no podemos cerrar explícitamente sin hacer drop del Environment
        // El Environment se cerrará automáticamente cuando el struct se dropee
        info!("Database connection will be closed when AppDbState is dropped");
        Ok(())
    }
}