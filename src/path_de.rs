use serde::de::DeserializeOwned;

/// Deserialize with JSON-path context in error messages.
pub fn from_str_with_path<T: DeserializeOwned>(src: &str) -> Result<T, String> {
    let de = &mut serde_json::Deserializer::from_str(src);
    match serde_path_to_error::deserialize::<_, T>(de) {
        Ok(v) => Ok(v),
        Err(err) => {
            let path = err.path().to_string();
            Err(format!("at JSON path {path} → {}", err.into_inner()))
        }
    }
}

pub fn from_slice_with_path<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let de = &mut serde_json::Deserializer::from_slice(bytes);
    match serde_path_to_error::deserialize::<_, T>(de) {
        Ok(v) => Ok(v),
        Err(err) => {
            let path = err.path().to_string();
            Err(format!("at JSON path {path} → {}", err.into_inner()))
        }
    }
}