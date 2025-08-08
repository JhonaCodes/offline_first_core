//! Database state management and operations.
//!
//! This module provides the core database functionality using LMDB (Lightning Memory-Mapped Database)
//! as the storage engine. It handles all database operations including initialization, CRUD operations,
//! and connection management.

use crate::local_db_model::LocalDbModel;
use log::{info, warn};
use lmdb::{Environment, Database, Transaction, WriteFlags, Cursor, DatabaseFlags, Error as LmdbError};
use std::fs;
use std::path::Path;
use crate::app_response::AppResponse;

/// The default database name within the LMDB environment.
const MAIN_DB_NAME: &str = "main";

/// Database state container that manages the LMDB environment and database connections.
///
/// This struct encapsulates the LMDB environment and database handle, providing
/// a safe interface for database operations. It maintains the database path for
/// operations like reset that require filesystem manipulation.
///
/// # Examples
///
/// ```no_run
/// use offline_first_core::local_db_state::AppDbState;
///
/// // Initialize a new database
/// let db_state = AppDbState::init("my_database".to_string())?;
///
/// // The database is ready for operations
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct AppDbState {
    /// LMDB environment handle (None when closed)
    env: Option<Environment>,
    /// Main database handle within the environment (None when closed)
    db: Option<Database>,
    /// Filesystem path to the database directory
    path: String,
}

impl AppDbState {
    /// Initializes a new database instance or opens an existing one.
    ///
    /// This function creates an LMDB environment with the specified name, setting up
    /// a directory-based storage system. The database is configured with a 1GB memory
    /// map size and support for up to 10 named databases.
    ///
    /// # Parameters
    ///
    /// * `name` - The base name for the database. A `.lmdb` extension will be added
    ///   to create the directory name.
    ///
    /// # Returns
    ///
    /// Returns `Ok(AppDbState)` on success, or `Err(LmdbError)` if initialization fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use offline_first_core::local_db_state::AppDbState;
    ///
    /// // Create or open a database named "user_data"
    /// let db = AppDbState::init("user_data".to_string())?;
    ///
    /// // The database directory will be "./user_data.lmdb"
    /// # Ok::<(), lmdb::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// - The database directory cannot be created
    /// - LMDB environment initialization fails
    /// - The main database cannot be created within the environment
    pub fn init(name: String) -> Result<Self, LmdbError> {
        let db_dir = format!("{name}.lmdb");
        let path = Path::new(&db_dir);
        
        if !path.exists() {
            fs::create_dir_all(path).map_err(|_| LmdbError::Other(2))?;
        }
        
        let env = Environment::new()
            .set_max_dbs(10)
            .set_map_size(1024 * 1024 * 1024) // 1GB
            .open(path)?;
        
        info!("LMDB environment opened at {name}");
        
        
        let db = match env.open_db(Some(MAIN_DB_NAME)) {
            Ok(data_db) => {
                info!("Found main database");
                data_db 
            },
            Err(_) => {
                info!("Creating main database");
                env.create_db(Some(MAIN_DB_NAME), DatabaseFlags::empty())?
            }
        }; 
        

        info!("Database initialized successfully");
        
        Ok(Self {
            env: Some(env),
            db: Some(db),
            path: db_dir
        })
    }

    /// Helper to get active environment and database handles.
    /// Returns error if the database has been explicitly closed.
    fn env_db(&self) -> Result<(&Environment, Database), LmdbError> {
        let env = self.env.as_ref().ok_or(LmdbError::Other(1))?;
        let db = self.db.as_ref().copied().ok_or(LmdbError::Other(1))?;
        Ok((env, db))
    }

    /// Inserts a new record into the database.
    ///
    /// This method serializes the provided model to JSON and stores it using the model's
    /// ID as the key. The operation is performed within a write transaction to ensure
    /// data consistency.
    ///
    /// # Parameters
    ///
    /// * `model` - The data model to insert into the database
    ///
    /// # Returns
    ///
    /// Returns the inserted model on success, or an error response if the operation fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use offline_first_core::{local_db_state::AppDbState, local_db_model::LocalDbModel};
    /// use serde_json::json;
    ///
    /// let db = AppDbState::init("test_db".to_string())?;
    ///
    /// let model = LocalDbModel {
    ///     id: "user_123".to_string(),
    ///     hash: "abc123".to_string(),
    ///     data: json!({"name": "John", "age": 30}),
    /// };
    ///
    /// let result = db.post(model)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// - JSON serialization fails
    /// - Transaction creation fails
    /// - Database write operation fails
    /// - Transaction commit fails
    pub fn post(&self, model: LocalDbModel) -> Result<LocalDbModel, AppResponse> {
        let json = serde_json::to_string(&model)?;

        let (env, db) = self.env_db().map_err(AppResponse::from)?;
        let mut txn = env.begin_rw_txn().map_err(AppResponse::from)?;
        txn.put(db, &model.id, &json, WriteFlags::empty()).map_err(AppResponse::from)?;
        txn.commit().map_err(AppResponse::from)?;

        Ok(model)
    }

    /// Retrieves a record from the database by its ID.
    ///
    /// This method performs a read-only lookup using the provided ID as the key.
    /// If found, the JSON data is deserialized back into a `LocalDbModel`.
    ///
    /// # Parameters
    ///
    /// * `id` - The unique identifier of the record to retrieve
    ///
    /// # Returns
    ///
    /// Returns `Ok(Some(LocalDbModel))` if the record is found, `Ok(None)` if not found,
    /// or `Err(LmdbError)` if the operation fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use offline_first_core::local_db_state::AppDbState;
    ///
    /// let db = AppDbState::init("test_db".to_string())?;
    ///
    /// match db.get_by_id("user_123")? {
    ///     Some(model) => println!("Found user: {:?}", model),
    ///     None => println!("User not found"),
    /// }
    /// # Ok::<(), lmdb::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// - Transaction creation fails
    /// - The stored data is not valid UTF-8
    /// - JSON deserialization fails
    pub fn get_by_id(&self, id: &str) -> Result<Option<LocalDbModel>, LmdbError> {
        let (env, db) = self.env_db()?;
        let txn = env.begin_ro_txn()?;
        
        match txn.get(db, &id) {
            Ok(bytes) => {
                let json_str = std::str::from_utf8(bytes)
                    .map_err(|_| LmdbError::Other(1))?;
                let model = serde_json::from_str(json_str)
                    .map_err(|_| LmdbError::Other(1))?;
                Ok(Some(model))
            }
            Err(LmdbError::NotFound) => {
                info!("No value found for id {id}");
                Ok(None)
            }
            Err(e) => Err(e)
        }
    }

    /// Retrieves all records from the database.
    ///
    /// This method iterates through all key-value pairs in the database,
    /// deserializing each JSON value back into a `LocalDbModel`. Records that
    /// fail to deserialize are logged and skipped.
    ///
    /// # Returns
    ///
    /// Returns a vector containing all successfully deserialized records,
    /// or an error if the database operation fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use offline_first_core::local_db_state::AppDbState;
    ///
    /// let db = AppDbState::init("test_db".to_string())?;
    ///
    /// let all_records = db.get()?;
    /// println!("Found {} records", all_records.len());
    ///
    /// for record in all_records {
    ///     println!("Record ID: {}", record.id);
    /// }
    /// # Ok::<(), lmdb::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// - Transaction creation fails
    /// - Cursor creation fails
    pub fn get(&self) -> Result<Vec<LocalDbModel>, LmdbError> {
        let mut models = Vec::new();
        
        let (env, db) = self.env_db()?;
        let txn = env.begin_ro_txn()?;
        let mut cursor = txn.open_ro_cursor(db)?;
        
        for (_, value) in cursor.iter() {
            match std::str::from_utf8(value) {
                Ok(json_str) => {
                    match serde_json::from_str::<LocalDbModel>(json_str) {
                        Ok(model) => models.push(model),
                        Err(e) => info!("Error deserializing model: {e:?}"),
                    }
                }
                Err(e) => info!("Error converting to UTF-8: {e:?}"),
            }
        }
        
        Ok(models)
    }

    /// Deletes a record from the database by its ID.
    ///
    /// This method first checks if the record exists, then removes it if found.
    /// The operation is performed within a write transaction for consistency.
    ///
    /// # Parameters
    ///
    /// * `id` - The unique identifier of the record to delete
    ///
    /// # Returns
    ///
    /// Returns `true` if a record was deleted, `false` if no record with the given ID exists,
    /// or an error if the operation fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use offline_first_core::local_db_state::AppDbState;
    ///
    /// let db = AppDbState::init("test_db".to_string())?;
    ///
    /// match db.delete_by_id("user_123")? {
    ///     true => println!("Record deleted successfully"),
    ///     false => println!("Record not found"),
    /// }
    /// # Ok::<(), lmdb::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// - Transaction creation fails
    /// - Database operations fail
    /// - Transaction commit fails
    pub fn delete_by_id(&self, id: &str) -> Result<bool, LmdbError> {
        let (env, db) = self.env_db()?;
        let mut txn = env.begin_rw_txn()?;
        
        let existed = match txn.get(db, &id) {
            Ok(_) => true,
            Err(LmdbError::NotFound) => false,
            Err(e) => return Err(e),
        };
        
        if existed {
            txn.del(db, &id, None)?;
        }
        
        txn.commit()?;
        Ok(existed)
    }

    /// Updates an existing record in the database.
    ///
    /// This method first verifies that a record with the given ID exists, then
    /// updates it with the new data. If no record exists, the operation returns `None`.
    ///
    /// # Parameters
    ///
    /// * `model` - The updated model data. The ID field determines which record to update.
    ///
    /// # Returns
    ///
    /// Returns `Some(LocalDbModel)` with the updated data if successful, `None` if no
    /// record with the given ID exists, or an error if the operation fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use offline_first_core::{local_db_state::AppDbState, local_db_model::LocalDbModel};
    /// use serde_json::json;
    ///
    /// let db = AppDbState::init("test_db".to_string())?;
    ///
    /// let updated_model = LocalDbModel {
    ///     id: "user_123".to_string(),
    ///     hash: "new_hash".to_string(),
    ///     data: json!({"name": "Jane", "age": 25}),
    /// };
    ///
    /// match db.put(updated_model)? {
    ///     Some(model) => println!("Updated: {:?}", model),
    ///     None => println!("Record not found for update"),
    /// }
    /// # Ok::<(), lmdb::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// - Transaction creation fails
    /// - JSON serialization fails
    /// - Database operations fail
    /// - Transaction commit fails
    pub fn put(&self, model: LocalDbModel) -> Result<Option<LocalDbModel>, LmdbError> {
        let (env, db) = self.env_db()?;
        let mut txn = env.begin_rw_txn()?;
        
        let exists = match txn.get(db, &model.id) {
            Ok(_) => true,
            Err(LmdbError::NotFound) => false,
            Err(e) => return Err(e),
        };
        
        if exists {
            let json = serde_json::to_string(&model)
                .map_err(|_| LmdbError::Other(1))?;
            txn.put(db, &model.id, &json, WriteFlags::empty())?;
            txn.commit()?;
            Ok(Some(model))
        } else {
            Ok(None)
        }
    }

    /// Removes all records from the database while preserving the database structure.
    ///
    /// This method iterates through all records and deletes them individually.
    /// The database remains operational after this operation and can continue
    /// to accept new records.
    ///
    /// # Returns
    ///
    /// Returns the number of records that were deleted, or an error if the operation fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use offline_first_core::local_db_state::AppDbState;
    ///
    /// let db = AppDbState::init("test_db".to_string())?;
    ///
    /// let deleted_count = db.clear_all_records()?;
    /// println!("Deleted {} records", deleted_count);
    /// # Ok::<(), lmdb::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// - Transaction creation fails
    /// - Cursor operations fail
    /// - Delete operations fail
    /// - Transaction commit fails
    pub fn clear_all_records(&self) -> Result<usize, LmdbError> {
        let (env, db) = self.env_db()?;
        let mut txn = env.begin_rw_txn()?;
        let mut count = 0;
        
        let keys: Vec<Vec<u8>> = {
            let mut cursor = txn.open_ro_cursor(db)?;
            cursor.iter()
                .map(|(key, _)| key.to_vec())
                .collect()
        };
        
        for key in keys {
            match txn.del(db, &key, None) {
                Ok(_) => count += 1,
                Err(e) => warn!("Error deleting key: {e:?}"),
            }
        }
        txn.commit()?;
        Ok(count)
    }

    /// Completely resets the database to a clean state with a new name.
    ///
    /// This operation performs the following steps:
    /// 1. Closes the current database environment
    /// 2. Removes the existing database directory and all its contents
    /// 3. Creates a new database environment with the specified name
    /// 4. Updates the internal state to use the new database
    ///
    /// # Parameters
    ///
    /// * `name` - The new name for the database
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` on success, or an error if any step fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use offline_first_core::local_db_state::AppDbState;
    ///
    /// let mut db = AppDbState::init("old_db".to_string())?;
    ///
    /// // Reset to a new database
    /// db.reset_database("new_db")?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// - The existing database directory cannot be removed
    /// - The new database directory cannot be created
    /// - LMDB environment initialization fails
    /// - Database creation within the environment fails
    ///
    /// # Safety
    ///
    /// This operation is destructive and will permanently delete all data in the current database.
    /// Ensure that any important data is backed up before calling this method.
    pub fn reset_database(&mut self, name: &str) -> Result<bool, Box<dyn std::error::Error>> {
        self.close_database()?;
        if Path::new(&self.path).exists() {
            fs::remove_dir_all(&self.path)?;
        }
        
        let new_db_dir = format!("{name}.lmdb");
        let path = Path::new(&new_db_dir);
        
        if !path.exists() {
            fs::create_dir_all(path)?;
        }
        
        let new_env = Environment::new()
            .set_max_dbs(10)
            .set_map_size(1024 * 1024 * 1024)
            .open(path)?;
            
        let new_db = new_env.create_db(Some(MAIN_DB_NAME), DatabaseFlags::empty())?;
        
        self.env = Some(new_env);
        self.db = Some(new_db);
        self.path = new_db_dir;
        
        Ok(true)
    }
    
    /// Provides explicit database connection management.
    ///
    /// This method serves as an explicit indicator that database resources should be
    /// cleaned up. While LMDB automatically closes connections when the environment
    /// is dropped, this function provides a clear signal for connection lifecycle
    /// management, particularly useful in FFI scenarios like Flutter hot restart.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on success. This operation cannot fail as it only provides
    /// a signal for cleanup rather than performing actual resource deallocation.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use offline_first_core::local_db_state::AppDbState;
    ///
    /// let mut db = AppDbState::init("test_db".to_string())?;
    ///
    /// // Before hot restart or application shutdown
    /// db.close_database()?;
    /// # Ok::<(), lmdb::Error>(())
    /// ```
    ///
    /// # Notes
    ///
    /// In LMDB, database connections are automatically managed through RAII.
    /// The actual cleanup occurs when the `AppDbState` instance is dropped.
    /// This method primarily serves as documentation and explicit lifecycle management
    /// for integration scenarios.
    pub fn close_database(&mut self) -> Result<(), LmdbError> {
        if let Some(env) = self.env.take() {
            // Best-effort sync before closing
            if let Err(e) = env.sync(true) {
                warn!("Failed to sync LMDB env before close: {e:?}");
            }
            drop(env);
        }
        self.db = None;
        info!("LMDB environment closed");
        Ok(())
    }
}
