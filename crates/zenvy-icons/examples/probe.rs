//! Developer tool: resolve icons for window classes on this machine. Usage: probe class...
fn main() {
    let r = zenvy_icons::IconResolver::from_env();
    for c in std::env::args().skip(1) {
        match r.resolve(&c) {
            Some(i) => println!("{c}: {} {} bytes", i.mime, i.bytes.len()),
            None => println!("{c}: none"),
        }
    }
}
