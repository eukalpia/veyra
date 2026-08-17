use crate::JournalError;

impl PartialEq for JournalError {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
}
