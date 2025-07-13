# 🗃️ Offline First Core

[![Crates.io](https://img.shields.io/crates/v/offline_first_core.svg)](https://crates.io/crates/offline_first_core)
[![Documentation](https://docs.rs/offline_first_core/badge.svg)](https://docs.rs/offline_first_core)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

High-performance LMDB-based local storage library optimized for FFI integration with Flutter and cross-platform applications.

## ✨ Features

- 🚀 **LMDB-powered** - Battle-tested database engine used by Bitcoin Core and OpenLDAP
- 📱 **Flutter-ready** - Hot restart compatible FFI interface
- ⚡ **High performance** - Zero-copy reads and ACID transactions
- 🔄 **Cross-platform** - Works on iOS, Android, Windows, macOS, and Linux
- 📦 **Simple API** - Only 9 functions to learn

## 🚀 Quick Start

### Flutter Integration

```dart
import 'dart:ffi';
import 'dart:convert';

// 1. Load the native library
final dylib = DynamicLibrary.open('liboffline_first_core.so');

// 2. Define FFI functions
typedef CreateDbNative = Pointer Function(Pointer<Utf8>);
typedef CreateDb = Pointer Function(Pointer<Utf8>);
final createDb = dylib.lookupFunction<CreateDbNative, CreateDb>('create_db');

// 3. Use the database
final dbPointer = createDb("my_app_database".toNativeUtf8());

final jsonData = jsonEncode({
  "id": "user_123",
  "hash": "content_hash",
  "data": {"name": "John Doe", "email": "john@example.com"}
});

final result = pushData(dbPointer, jsonData.toNativeUtf8());
```

### Rust Direct Usage

```rust
use offline_first_core::{AppDbState, LocalDbModel};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize database
    let db = AppDbState::init("my_database".to_string())?;

    // Create and store data
    let user = LocalDbModel {
        id: "user_123".to_string(),
        hash: "content_hash".to_string(),
        data: json!({"name": "John Doe", "email": "john@example.com"}),
    };

    db.push(user)?;

    // Retrieve data
    let user = db.get_by_id("user_123")?.unwrap();
    println!("User: {}", user.data["name"]);

    Ok(())
}
```

## 📚 API Reference

### Core Functions

| Function | Rust | FFI | Description |
|----------|------|-----|-------------|
| **Initialize** | `AppDbState::init(name)` | `create_db(name)` | Create or open database |
| **Insert** | `db.push(model)` | `push_data(db, json)` | Add new record |
| **Get by ID** | `db.get_by_id(id)` | `get_by_id(db, id)` | Retrieve specific record |
| **Get All** | `db.get()` | `get_all(db)` | Retrieve all records |
| **Update** | `db.update(model)` | `update_data(db, json)` | Update existing record |
| **Delete** | `db.delete_by_id(id)` | `delete_by_id(db, id)` | Remove record |
| **Clear** | `db.clear_all_records()` | `clear_all_records(db)` | Remove all records |
| **Reset** | `db.reset_database(name)` | `reset_database(db, name)` | Reset database |
| **Close** | `db.close_database()` | `close_database(db)` | Close connection |

### Data Model

```rust
pub struct LocalDbModel {
    pub id: String,      // Unique identifier (cannot be empty)
    pub hash: String,    // Content hash for versioning
    pub data: JsonValue, // Your JSON data
}
```

## 🎯 Usage Examples

### 1. User Preferences

```rust
use offline_first_core::{AppDbState, LocalDbModel};
use serde_json::json;

fn save_preferences() -> Result<(), Box<dyn std::error::Error>> {
    let db = AppDbState::init("user_settings".to_string())?;
    
    let preferences = LocalDbModel {
        id: "app_preferences".to_string(),
        hash: "v1.0".to_string(),
        data: json!({
            "theme": "dark",
            "language": "en",
            "notifications": true
        }),
    };
    
    db.push(preferences)?;
    Ok(())
}
```

### 2. Shopping Cart

```rust
fn add_to_cart(product_id: &str, quantity: i32) -> Result<(), Box<dyn std::error::Error>> {
    let db = AppDbState::init("shopping_cart".to_string())?;
    
    let item = LocalDbModel {
        id: product_id.to_string(),
        hash: format!("cart_{}", chrono::Utc::now().timestamp()),
        data: json!({
            "product_id": product_id,
            "quantity": quantity,
            "price": 29.99
        }),
    };
    
    db.push(item)?;
    Ok(())
}
```

### 3. Offline Cache

```rust
fn cache_article(article_id: &str, content: &str) -> Result<(), Box<dyn std::error::Error>> {
    let db = AppDbState::init("article_cache".to_string())?;
    
    let article = LocalDbModel {
        id: article_id.to_string(),
        hash: format!("article_{}", md5::compute(content)),
        data: json!({
            "title": "Article Title",
            "content": content,
            "cached_at": chrono::Utc::now().to_rfc3339()
        }),
    };
    
    db.push(article)?;
    Ok(())
}
```

## 🔧 Advanced Usage

### Error Handling

```rust
use offline_first_core::{AppDbState, AppResponse};

match db.push(model) {
    Ok(_) => println!("✅ Success"),
    Err(AppResponse::DatabaseError(msg)) => eprintln!("💾 Database error: {}", msg),
    Err(AppResponse::SerializationError(msg)) => eprintln!("📝 JSON error: {}", msg),
    Err(AppResponse::NotFound(msg)) => eprintln!("🔍 Not found: {}", msg),
    Err(other) => eprintln!("🔥 Other error: {}", other),
}
```

### Batch Operations

```rust
fn bulk_insert(records: Vec<LocalDbModel>) -> Result<usize, Box<dyn std::error::Error>> {
    let db = AppDbState::init("bulk_data".to_string())?;
    let mut success_count = 0;
    
    for model in records {
        if db.push(model).is_ok() {
            success_count += 1;
        }
    }
    
    Ok(success_count)
}
```

### Hot Restart (Flutter)

```dart
class DatabaseManager {
  static Pointer? _dbPointer;
  
  static void initDatabase() {
    _dbPointer = createDb("my_app_db".toNativeUtf8());
  }
  
  static void closeDatabase() {
    if (_dbPointer != null) {
      closeDatabase(_dbPointer!);
      _dbPointer = null;
    }
  }
}
```

## 🛠️ Setup

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
offline_first_core = "0.3.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

For FFI projects:

```toml
[lib]
name = "my_storage_lib"
crate-type = ["staticlib", "cdylib"]
```

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Cross-platform builds
cargo build --target aarch64-apple-ios --release        # iOS
cargo build --target aarch64-linux-android --release    # Android
cargo build --target x86_64-pc-windows-msvc --release   # Windows
```

## ⚠️ Important Notes

### LMDB Limitations

```rust
// ❌ Empty IDs not supported
let invalid = LocalDbModel {
    id: "".to_string(), // Will fail!
    // ...
};

// ✅ Always use non-empty IDs
let valid = LocalDbModel {
    id: "user_123".to_string(), // Good!
    // ...
};
```

### Memory Safety (FFI)

```c
// ✅ Always check null pointers
void* db = create_db("my_db");
if (db == NULL) {
    // Handle error
    return;
}

// ✅ Free returned strings
const char* result = get_by_id(db, "user_1");
if (result != NULL) {
    // Use result...
    free((void*)result); // Important!
}
```

### Performance Tips

```rust
// ✅ DO: Reuse connections
let db = AppDbState::init("my_db".to_string())?;
for i in 0..1000 {
    db.push(create_model(i))?; // Efficient
}

// ❌ DON'T: Create new connections
for i in 0..1000 {
    let db = AppDbState::init("my_db".to_string())?; // Slow!
    db.push(create_model(i))?;
}
```

## 🧪 Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_basic_crud() {
        let db = AppDbState::init("test_db".to_string()).unwrap();
        
        let model = LocalDbModel {
            id: "test_1".to_string(),
            hash: "test_hash".to_string(),
            data: json!({"name": "Test User"}),
        };
        
        // Insert
        assert!(db.push(model).is_ok());
        
        // Read
        let retrieved = db.get_by_id("test_1").unwrap();
        assert!(retrieved.is_some());
        
        // Update
        let mut updated = retrieved.unwrap();
        updated.data["name"] = json!("Updated User");
        assert!(db.update(updated).is_ok());
        
        // Delete
        assert!(db.delete_by_id("test_1").unwrap());
    }
}
```

## 📦 Integration Examples

### Flutter Plugin

```dart
// pubspec.yaml
dependencies:
  ffi: ^2.0.0

// lib/database.dart
import 'dart:ffi';
import 'dart:io';

class NativeDatabase {
  late DynamicLibrary _lib;
  
  NativeDatabase() {
    if (Platform.isAndroid) {
      _lib = DynamicLibrary.open('liboffline_first_core.so');
    } else if (Platform.isIOS) {
      _lib = DynamicLibrary.process();
    }
  }
  
  // Define your FFI functions here...
}
```

### React Native

```javascript
// Install react-native-ffi
npm install react-native-ffi

// Use the library
import { NativeModules } from 'react-native';
const { OfflineFirstCore } = NativeModules;

async function saveData(id, data) {
  const result = await OfflineFirstCore.pushData(id, JSON.stringify(data));
  return JSON.parse(result);
}
```

## 📋 Changelog

### v0.3.0 - LMDB Migration
- ✨ Migrated from redb to LMDB
- ✨ Added `close_database()` function
- 🐛 Fixed Flutter hot restart issues
- 🔧 Improved error handling
- 📚 Added comprehensive test suite (60+ tests)

### v0.2.0 - Feature Expansion
- ✨ Added `clear_all_records()` and `reset_database()`
- 🔧 Improved error handling
- 🛡️ Enhanced FFI safety

### v0.1.0 - Initial Release
- ✨ Basic CRUD operations
- 🔌 FFI interface for cross-platform integration

## 🤝 Contributing

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- [LMDB team](https://www.symas.com/lmdb) for the robust database engine
- [redb](https://github.com/cberner/redb) project for inspiration
- Rust community for excellent FFI tools

---

<div align="center">

**Made with ❤️ for developers who need reliable offline storage**

[⭐ Star on GitHub](https://github.com/JhonaCodes/offline_first_core) • [📦 View on Crates.io](https://crates.io/crates/offline_first_core) • [📖 Read the Docs](https://docs.rs/offline_first_core)

</div>