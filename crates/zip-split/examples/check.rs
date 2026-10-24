//! check <last part>: read every entry of a split set, report CRC failures.
use std::io::Read;

fn main() {
    let path = std::env::args().nth(1).expect("usage: check <last part>");
    let set = spike_zip_split::SplitZip::open(std::path::Path::new(&path)).expect("open");
    let mut archive = zip::ZipArchive::new(set).expect("zip");
    let mut failed = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).expect("entry");
        let mut data = Vec::new();
        if let Err(err) = entry.read_to_end(&mut data) {
            failed += 1;
            println!("{}: {err}", entry.name().expect("name"));
        }
    }
    println!("{} entries checked, {failed} failed", archive.len());
}
