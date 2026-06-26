use rusqlite::ffi::sqlite3_auto_extension;
use sqlite_vec::sqlite3_vec_init;

static VEC_INIT: std::sync::Once = std::sync::Once::new();

#[allow(clippy::missing_transmute_annotations)]
pub fn init() {
    VEC_INIT.call_once(|| unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_registers_vec_functions() {
        init();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let version: String = conn
            .query_row("SELECT vec_version()", [], |r| r.get(0))
            .unwrap();
        assert!(!version.is_empty());
    }
}
