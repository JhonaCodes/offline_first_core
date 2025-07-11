use crate::local_db_model::LocalDbModel;
use log::{info, warn};
use redb::{
    Database, DatabaseError, Error, ReadableTable, ReadableTableMetadata, StorageError,
    TableDefinition, WriteTransaction, ReadTransaction,
};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};
use crate::app_response::AppResponse;

// Table definition for redb - required for key-value storage
const MAIN_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("main");

pub struct AppDbState {
    db: Database,
    path: String, // Store the path for potential database reset
}

impl Drop for AppDbState {
    fn drop(&mut self) {
        info!("Dropping AppDbState for database: {}", self.path);
        // ReDB automatically handles proper closure when Database is dropped
        // File locks and resources are properly released
    }
}

// Constants for transaction timeouts
const TRANSACTION_TIMEOUT: Duration = Duration::from_secs(10);
const LONG_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

impl AppDbState {
    pub fn init(name: String) -> Result<Self, DatabaseError> {
        let path = Path::new(&name);

        // Abrir la base de datos o crearla si no existe
        let db = match Database::open(path) {
            Ok(response) => {
                info!("Opened existing database at {}", name);
                response
            }
            Err(_) => {
                info!("Creating new database at {}", name);
                match Database::create(path) {
                    Ok(response) => {
                        info!("Database created");
                        response
                    }
                    Err(err) => {
                        warn!("Error on creating database: {}", err);
                        return Err(DatabaseError::Storage(StorageError::Corrupted(String::from("Error when trying to create database"))));
                    }
                }
            }
        };

        // Iniciar transacción de escritura
        let write_txn = match db.begin_write() {
            Ok(txn) => txn,
            Err(err) => {
                warn!("Error beginning write transaction: {}", err);
                return Err(DatabaseError::Storage(StorageError::Corrupted(String::from("Error beginning write transaction"))));
            }
        };

        // Abrir o crear tabla
        match write_txn.open_table(MAIN_TABLE) {
            Ok(_) => {
                info!("Table opened successfully")
            },
            Err(err) => {
                warn!("Error opening table: {}", err);
                return Err(DatabaseError::Storage(StorageError::Corrupted(String::from("Error opening table"))));
            }
        }

        // Confirmar transacción
        match write_txn.commit() {
            Ok(_) => {
                info!("Transaction committed successfully")
            },
            Err(err) => {
                warn!("Error committing transaction: {}", err);
                return Err(DatabaseError::Storage(StorageError::Corrupted(String::from("Error committing transaction"))));
            }
        }

        Ok(Self {
            db,
            path: name
        })
    }

    pub fn push(&self, model: LocalDbModel) -> Result<LocalDbModel, AppResponse> {
        let json = serde_json::to_string(&model)?;
        let start_time = Instant::now();

        let write_txn = self.db.begin_write().map_err(AppResponse::from)?;
        
        let result = {
            // Check timeout before proceeding
            if start_time.elapsed() > TRANSACTION_TIMEOUT {
                return Err(AppResponse::DatabaseError("Transaction timeout during initialization".to_string()));
            }

            let mut table = write_txn.open_table(MAIN_TABLE).map_err(AppResponse::from)?;
            
            // Check timeout before insert
            if start_time.elapsed() > TRANSACTION_TIMEOUT {
                return Err(AppResponse::DatabaseError("Transaction timeout before insert".to_string()));
            }
            
            table.insert(model.id.as_str(), json.as_bytes()).map_err(AppResponse::from)?;
            model
        };

        // Check timeout before commit
        if start_time.elapsed() > TRANSACTION_TIMEOUT {
            return Err(AppResponse::DatabaseError("Transaction timeout before commit".to_string()));
        }

        write_txn.commit().map_err(AppResponse::from)?;
        
        // Log if operation took long
        let elapsed = start_time.elapsed();
        if elapsed > Duration::from_millis(100) {
            warn!("Push operation took {}ms", elapsed.as_millis());
        }

        Ok(result)
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<LocalDbModel>, Error> {
        let start_time = Instant::now();
        let read_txn = self.db.begin_read()?;
        
        // Check timeout after transaction start
        if start_time.elapsed() > TRANSACTION_TIMEOUT {
            warn!("Read transaction timeout for id: {}", id);
            return Err(Error::Corrupted("Read timeout".to_string()));
        }
        
        let table = read_txn.open_table(MAIN_TABLE)?;

        let result = match table.get(id)? {
            Some(bytes) => {
                // Check timeout before processing data
                if start_time.elapsed() > TRANSACTION_TIMEOUT {
                    warn!("Data processing timeout for id: {}", id);
                    return Err(Error::Corrupted("Processing timeout".to_string()));
                }
                
                let json_str = String::from_utf8(bytes.value().to_vec()).unwrap();
                let model = serde_json::from_str(&json_str).unwrap();
                Ok(Some(model))
            }
            None => {
                info!("No value found for id {}", id);
                Ok(None)
            }
        };

        // Log if operation took long
        let elapsed = start_time.elapsed();
        if elapsed > Duration::from_millis(50) {
            warn!("Get by id operation took {}ms", elapsed.as_millis());
        }

        result
    }

    pub fn get(&self) -> Result<Vec<LocalDbModel>, redb::Error> {
        let mut models = Vec::new();

        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(MAIN_TABLE)?;

        for item in table.iter()? {
            match item {
                Ok((_, value)) => {
                    let json_str = String::from_utf8(Vec::from(value.value())).unwrap();
                    let model: LocalDbModel = serde_json::from_str(&json_str).unwrap();
                    models.push(model);
                }
                Err(e) => info!("Rust: Error reading item: {:?}", e),
            }
        }

        Ok(models)
    }

    pub fn delete_by_id(&self, id: &str) -> Result<bool, redb::Error> {
        let write_txn = self.db.begin_write()?;
        let mut table = write_txn.open_table(MAIN_TABLE)?;
        let existed = table.remove(id)?.is_some();
        drop(table); // Explícitamente liberamos la tabla
        write_txn.commit()?;
        Ok(existed)
    }

    pub fn update(&self, model: LocalDbModel) -> Result<Option<LocalDbModel>, redb::Error> {
        let write_txn = self.db.begin_write()?;
        let result = {
            let mut table = write_txn.open_table(MAIN_TABLE)?;

            // Check if exists
            if table.get(model.id.as_str())?.is_some() {
                let json = serde_json::to_string(&model).unwrap();
                table.insert(model.id.as_str(), json.as_bytes())?;
                Some(model)
            } else {
                None
            }
        }; // La tabla se libera aquí
        write_txn.commit()?;
        Ok(result)
    }

    /// Deletes all records from the database while maintaining the database structure
    /// Returns the number of records deleted
    /// This is useful when you want to clear data but keep using the same database
    pub fn clear_all_records(&self) -> Result<usize, redb::Error> {
        let write_txn = self.db.begin_write()?; // Iniciar transacción de escritura
        let mut count = 0;

        {
            let mut table = write_txn.open_table(MAIN_TABLE)?;

            if table.is_empty()? {
                return Ok(0);
            }

            let keys: Vec<String> = table
                .iter()?
                .filter_map(|entry| entry.ok())
                .map(|(k, _)| k.value().to_string())
                .collect();

            for key in keys {
                if let Err(e) = table.remove(key.as_str()) {
                    warn!("Error on deleting key: {:?}", e);
                } else {
                    count += 1;
                }
            }
        }

        write_txn.commit()?;
        Ok(count)
    }

    /// Completely resets the database by:
    /// 1. Closing the current connection
    /// 2. Deleting the database file
    /// 3. Creating a new database
    /// This is useful when you want to start completely fresh
    /// Returns true if successful
    pub fn reset_database(&mut self, name: &str) -> Result<bool, Box<dyn std::error::Error>> {
        // Delete the database file
        fs::remove_file(&self.path)?;

        // Create a new database
        let path = Path::new(name);
        let new_db = Database::create(path)?;

        // Initialize the table structure
        {
            let write_txn = new_db.begin_write()?;
            write_txn.open_table(MAIN_TABLE)?;
            write_txn.commit()?;
        }

        // Update our database reference
        self.db = new_db;

        Ok(true)
    }
}
