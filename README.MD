# Native Storage Library

A high-performance local storage library written in Rust, designed for FFI (Foreign Function Interface) integration. This library provides a robust key-value storage solution using redb, optimized for embedded systems and mobile applications.

## Features

- 🔋 Embedded key-value storage powered by redb
- 🔒 ACID compliant transactions
- 🚀 Zero-copy reads
- 📱 Optimized for mobile and embedded systems
- 🛡️ Safe FFI interface
- 🔄 Full CRUD operations
- 🧪 Comprehensive error handling

## Usage

### C FFI Interface

The library exposes the following FFI functions for integration with other languages:

```c
void* create_db(const char* name);
const char* push_data(void* state, const char* json_ptr);
const char* get_by_id(void* state, const char* id);
const char* get_all(void* state);
const char* update_data(void* state, const char* json_ptr);
bool delete_by_id(void* state, const char* id);
```

### Direct Rust Usage

```rust
use native_storage::{AppDbState, LocalDbModel};

// Initialize database
let db = AppDbState::init("my_database.db".to_string());

// Create data
let model = LocalDbModel {
    id: "unique_id".to_string(),
    // ... other fields
};

// Store data
db.push(model)?;

// Retrieve data
let item = db.get_by_id("unique_id")?;
let all_items = db.get()?;

// Update data
db.update(updated_model)?;

// Delete data
db.delete_by_id("unique_id")?;

// Clear database
db.clear_all_records()?;

// Reset database
db.reset_database()?;
```

## Implementation Details

### Core Components

- `AppDbState`: Main database handler
- `LocalDbModel`: Data model for storage
- FFI layer for C interface

### Storage Engine

Uses redb with the following configuration:
- Key-value storage
- ACID transactions
- Single table design
- JSON serialization for values

### Data Model

Data is stored as JSON strings with the following characteristics:
- String keys (IDs)
- Serialized JSON values
- UTF-8 encoding

### Error Handling

- Comprehensive error handling for all operations
- Safe FFI error propagation
- Null pointer checks
- JSON parsing validation

## Building

### Prerequisites

- Rust 1.75.0 or later
- Cargo
- C compiler (for FFI)

### Build Commands

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Generate C headers
cbindgen --output native_storage.h
```

### Integration

To integrate with your project:

1. Add to your `Cargo.toml`:
```toml
[dependencies]
redb = "1.0.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

2. Link with your application:
```toml
[lib]
name = "native_storage"
crate-type = ["staticlib", "cdylib"]
```

## Safety Considerations

- All FFI functions are marked with `#[no_mangle]`
- Proper memory management for C strings
- Safe handling of null pointers
- Transaction integrity protection
- Resource cleanup on drop

## Performance Notes

- Optimized for embedded systems
- Minimal memory footprint
- Efficient JSON serialization
- Transaction batching where possible

## Limitations

- Single table design
- Synchronous API
- JSON-only value storage
- No built-in encryption

## Contributing

Contributions are welcome! Please ensure:
1. Proper error handling
2. Memory safety
3. FFI compatibility
4. Documentation
5. Test coverage

## License

MIT License