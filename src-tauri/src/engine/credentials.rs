const SERVICE: &str = "e";

fn entry(provider_id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, &format!("provider:{provider_id}"))
        .map_err(|e| format!("could not open the OS credential store: {e}"))
}

pub fn load(provider_id: &str) -> Result<Option<String>, String> {
    match entry(provider_id)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!(
            "could not read the API key for provider '{provider_id}' from the OS credential store: {e}"
        )),
    }
}

pub fn save(provider_id: &str, value: &str) -> Result<(), String> {
    let entry = entry(provider_id)?;
    if value.is_empty() {
        return match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!(
                "could not delete the API key for provider '{provider_id}' from the OS credential store: {e}"
            )),
        };
    }
    entry.set_password(value).map_err(|e| {
        format!(
            "could not save the API key for provider '{provider_id}' in the OS credential store: {e}"
        )
    })
}

pub fn delete(provider_id: &str) -> Result<(), String> {
    save(provider_id, "")
}
