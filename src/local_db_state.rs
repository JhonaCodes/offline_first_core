use sled::Db;
use crate::local_db_model::LocalDbModel;

pub struct AppDbState{
    pub db: Db
}

impl AppDbState{
    pub fn init(name: String) -> Self {
        // Configuración más robusta según la documentación
        let config = sled::Config::new()
            .path(name)
            // Asegurar que los datos persistan
            .mode(sled::Mode::HighThroughput)
            // Flush más frecuente
            .flush_every_ms(Some(1000));

        let db = config.open().unwrap();
        // Forzar un flush inicial
        db.flush().unwrap();

        Self { db }
    }
    pub fn push(&self, model: LocalDbModel) -> Result<LocalDbModel, sled::Error> {
        let json = serde_json::to_string(&model).unwrap();
        self.db.insert(model.id.clone(), json.as_bytes())?;
        // Asegurar que se escriba a disco
        self.db.flush()?;
        Ok(model)
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<LocalDbModel>, sled::Error> {
        println!("Buscando id: {}", id);

        // Listar todas las claves en la DB para debug
        println!("Keys en la DB:");
        for item in self.db.iter() {
            if let Ok((key, _)) = item {
                println!("Key encontrada: {:?}", String::from_utf8(key.to_vec()));
            }
        }
        
        match self.db.get(id)? {
            Some(bytes) => {
                println!("Valor encontrado para id {}", id);
                let json_str = String::from_utf8(bytes.to_vec()).unwrap();
                println!("JSON recuperado: {}", json_str);
                let model = serde_json::from_str(&json_str).unwrap();
                Ok(Some(model))
            },
            None => {
                println!("No se encontró valor para id {}", id);
                Ok(None)
            }
        }
    }


    pub fn get(&self) -> Result<Vec<LocalDbModel>, sled::Error> {
        println!("Rust: Scanning database"); // Debug
        let mut models = Vec::new();

        // Imprimir todas las claves en la base de datos
        for item in self.db.iter() {
            match item {
                Ok((key, value)) => {
                    println!("Rust: Found key: {:?}", String::from_utf8(key.to_vec()));
                    let json_str = String::from_utf8(value.to_vec()).unwrap();
                    let model: LocalDbModel = serde_json::from_str(&json_str).unwrap();
                    models.push(model);
                },
                Err(e) => println!("Rust: Error reading item: {:?}", e)
            }
        }

        println!("Rust: Total models found: {}", models.len());
        Ok(models)
    }
    

    pub fn delete_by_id(&self, id: &str) -> Result<bool, sled::Error> {
        match self.db.remove(id)? {
            Some(_) => {
                self.db.flush()?;
                Ok(true) // Se encontró y eliminó
            },
            None => Ok(false) // No se encontró el registro
        }
    }


    pub fn update(&self, model: LocalDbModel) -> Result<Option<LocalDbModel>, sled::Error>  {
        // Verificar si existe
        if self.db.contains_key(&model.id)? {
            let json = serde_json::to_string(&model).unwrap();
            self.db.insert(model.id.clone(), json.as_bytes())?;
            self.db.flush()?;
            Ok(Some(model))
        } else {
            // Podríamos manejar esto de diferentes formas, aquí retornamos el error
            Ok(None)
        }
    }


}