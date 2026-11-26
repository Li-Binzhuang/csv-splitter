use crate::McpJson;

pub(super) fn flag_or(args: &McpJson, key: &str, default: bool) -> bool {
    match args.get(key) {
        Some(McpJson::Bool(value)) => *value,
        _ => default,
    }
}

pub(super) fn bounded(
    args: &McpJson,
    key: &str,
    min: u64,
    max: u64,
    default: u64,
) -> Result<u64, String> {
    match args.get(key) {
        None => Ok(default),
        Some(value) => {
            let number = value
                .as_u64()
                .ok_or_else(|| format!("{key} must be a non-negative integer"))?;
            if !(min..=max).contains(&number) {
                return Err(format!("{key} must be between {min} and {max}"));
            }
            Ok(number)
        }
    }
}

pub(super) fn optional_bounded(
    args: &McpJson,
    key: &str,
    min: u64,
    max: u64,
) -> Result<Option<u64>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(value) => {
            let number = value
                .as_u64()
                .ok_or_else(|| format!("{key} must be a non-negative integer"))?;
            if !(min..=max).contains(&number) {
                return Err(format!("{key} must be between {min} and {max}"));
            }
            Ok(Some(number))
        }
    }
}
