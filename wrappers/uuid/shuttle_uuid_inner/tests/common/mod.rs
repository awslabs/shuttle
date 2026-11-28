use shuttle::sync::{Arc, Mutex};
use shuttle::thread;
use shuttle_uuid_inner::Uuid;

/// Spawns `num_threads` threads that each create a UUID with `new_uuid`, and returns the UUIDs in
/// the order the threads created them, which depends on the schedule.
pub fn uuids_from_threads(num_threads: usize, new_uuid: fn() -> Uuid) -> Vec<Uuid> {
    let uuids = Arc::new(Mutex::new(Vec::new()));
    let threads = (0..num_threads)
        .map(|_| {
            let uuids = uuids.clone();
            thread::spawn(move || {
                let uuid = new_uuid();
                uuids.lock().unwrap().push(uuid);
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    uuids.lock().unwrap().clone()
}
