use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use rusqlite::{Connection, backup::Backup};

use crate::store::initialize_schema;

static TEMPLATE: OnceLock<Mutex<Connection>> = OnceLock::new();

pub fn database() -> Connection {
  let template = TEMPLATE.get_or_init(|| {
    let db = Connection::open_in_memory().expect("open test database template");
    db.execute_batch("pragma temp_store=memory;")
      .expect("keep test database temporary storage in memory");
    initialize_schema(&db).expect("initialize test database template");
    Mutex::new(db)
  });
  let template = template.lock().expect("lock test database template");
  let mut db = Connection::open_in_memory().expect("open isolated test database");
  {
    let backup = Backup::new(&template, &mut db).expect("start test database copy");
    backup
      .run_to_completion(100, Duration::ZERO, None)
      .expect("copy test database template");
  }
  db.execute_batch("pragma foreign_keys=on; pragma temp_store=memory;")
    .expect("configure isolated test database");
  db
}
