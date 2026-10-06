use anyhow::Result;
use sessidx::store::Store;
use std::{fs::File, path::Path, time::Instant};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(args.len() == 3, "profile_index MANIFEST DB");
    let manifest: serde_json::Value = serde_json::from_reader(File::open(&args[1])?)?;
    let mut store = Store::open(Path::new(&args[2]))?;
    let start = Instant::now();
    for item in manifest
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("manifest array required"))?
    {
        store.index_frozen(
            item["harness"].as_str().unwrap(),
            Path::new(item["path"].as_str().unwrap()),
            item["size"].as_u64().unwrap(),
        )?;
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        let rss = unsafe {
            anyhow::ensure!(
                libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) == 0,
                "getrusage failed"
            );
            usage.assume_init().ru_maxrss
        };
        let sqlite = unsafe { rusqlite::ffi::sqlite3_memory_highwater(0) };
        let mut malloc = std::mem::MaybeUninit::<libc::malloc_statistics_t>::uninit();
        let malloc = unsafe {
            libc::malloc_zone_statistics(std::ptr::null_mut(), malloc.as_mut_ptr());
            malloc.assume_init()
        };
        println!(
            "{}",
            serde_json::json!({"path":item["path"],"seconds":start.elapsed().as_secs_f64(),"peak_rss_bytes":rss,"sqlite_peak_bytes":sqlite,"heap_in_use_bytes":malloc.size_in_use,"heap_allocated_bytes":malloc.size_allocated})
        );
    }
    Ok(())
}
