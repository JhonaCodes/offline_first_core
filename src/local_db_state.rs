use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use crate::local_db_model::LocalDbModel;
use std::path::Path;
use std::fs;

// Table definition for redb - required for key-value storage
const MAIN_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("main");

pub struct AppDbState {
    db: Database,
    path: String, // Store the path for potential database reset
}

impl AppDbState {
    pub fn init(name: String) -> Self {
        let path = Path::new(&name);
        let db = Database::create(path).unwrap();

        // Create table if it doesn't exist
        {
            let write_txn = db.begin_write().unwrap();
            write_txn.open_table(MAIN_TABLE).unwrap();
            write_txn.commit().unwrap();
        }

        Self {
            db,
            path: name
        }
    }

    pub fn push(&self, model: LocalDbModel) -> Result<LocalDbModel, redb::Error> {
        let json = serde_json::to_string(&model).unwrap();
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(MAIN_TABLE)?;
            table.insert(model.id.as_str(), json.as_bytes())?;
        }
        write_txn.commit()?;
        Ok(model)
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<LocalDbModel>, redb::Error> {

        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(MAIN_TABLE)?;

        match table.get(id)? {
            Some(bytes) => {
                let json_str = String::from_utf8(bytes.value().to_vec()).unwrap();
                let model = serde_json::from_str(&json_str).unwrap();
                Ok(Some(model))
            },
            None => {
                println!("No value found for id {}", id);
                Ok(None)
            }
        }
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
                },
                Err(e) => println!("Rust: Error reading item: {:?}", e)
            }
        }
        
        Ok(models)
    }

    pub fn delete_by_id(&self, id: &str) -> Result<bool, redb::Error> {
        let write_txn = self.db.begin_write()?;
        let mut table = write_txn.open_table(MAIN_TABLE)?;
        let existed = table.remove(id)?.is_some();
        drop(table);  // Explícitamente liberamos la tabla
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
                    eprintln!("Error on deleting key: {:?}", e);
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